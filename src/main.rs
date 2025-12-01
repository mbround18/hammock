mod caption_sink_json;
mod config;
mod discord_utils;
mod transcription;
mod voice;

use std::{
    env,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use async_trait::async_trait;
use dashmap::DashMap;
use dotenvy::dotenv;
use futures_util::StreamExt;
use poise::{FrameworkOptions, builtins, serenity_prelude as serenity};
use reqwest::Client as HttpClient;
use tokio::{fs, io::AsyncWriteExt, process::Command, sync::oneshot, time::timeout};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    caption_sink_json::CaptionSink,
    config::BotConfig,
    discord_utils::resolve_user_name,
    transcription::{TranscriptionHandle, spawn_worker},
    voice::{
        CaptionPipelineConfig, SpeakerUpdateReceiver, SpeakerUpdateSender, attach_caption_pipeline,
        speaker_update_channel,
    },
};
use serenity::{
    Client as DiscordClient,
    gateway::ActivityData,
    model::{
        id::{ChannelId, GuildId, UserId},
        permissions::Permissions,
        prelude::{Mentionable, OnlineStatus},
    },
    prelude::GatewayIntents,
};
use songbird::{
    Call, Config as SongbirdConfig, SerenityInit,
    driver::{Channels as DecodeChannels, CryptoMode, DecodeMode, SampleRate as DecodeSampleRate},
    events::{Event, EventContext, EventHandler, TrackEvent},
    input::File as SongbirdFile,
};

type Error = anyhow::Error;
type Data = Arc<BotState>;
type BotContext<'a> = poise::Context<'a, Data, Error>;
type CallLock = Arc<tokio::sync::Mutex<Call>>;

const WHISPER_CPP_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const INVITE_SCOPES: &str = "bot%20applications.commands";
const ENTRY_SOUND_TIMEOUT: Duration = Duration::from_secs(30);
const ENTRY_SOUND_VOLUME: f32 = 0.5;

struct BotState {
    chunk_samples: usize,
    sample_rate: u32,
    transcriber: TranscriptionHandle,
    speaker_updates: SpeakerUpdateSender,
    caption_sink: Arc<CaptionSink>,
    entry_sound_path: PathBuf,
    active_calls: DashMap<GuildId, ChannelId>,
}

impl BotState {
    fn new(
        chunk_samples: usize,
        sample_rate: u32,
        transcriber: TranscriptionHandle,
        speaker_updates: SpeakerUpdateSender,
        caption_sink: Arc<CaptionSink>,
        entry_sound_path: PathBuf,
    ) -> Self {
        Self {
            chunk_samples,
            sample_rate,
            transcriber,
            speaker_updates,
            caption_sink,
            entry_sound_path,
            active_calls: DashMap::new(),
        }
    }

    fn speaker_updates(&self) -> SpeakerUpdateSender {
        self.speaker_updates.clone()
    }

    fn track_call(&self, guild_id: GuildId, channel_id: ChannelId) {
        self.active_calls.insert(guild_id, channel_id);
    }

    fn take_call_channel(&self, guild_id: GuildId) -> Option<ChannelId> {
        self.active_calls
            .remove(&guild_id)
            .map(|(_, channel)| channel)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let allow_udp_errors = env::var("ALLOW_SONGBIRD_UDP_ERRORS").ok().as_deref() == Some("1");
    let udp_rx_filter = tracing_subscriber::filter::FilterFn::new(move |meta| {
        if meta.target().contains("songbird::driver::tasks::udp_rx") {
            allow_udp_errors
        } else {
            true
        }
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(udp_rx_filter)
        .with(fmt::layer())
        .init();

    let config = BotConfig::from_env()?;
    // Ensure captions folder exists on startup
    if let Err(e) = std::fs::create_dir_all(&config.caption_dir) {
        tracing::error!(?e, "Failed to create caption output directory");
    }
    let (speaker_updates, speaker_rx) = speaker_update_channel();
    let speaker_rx = Arc::new(StdMutex::new(Some(speaker_rx)));
    ensure_model_available(&config).await?;
    let caption_sink = Arc::new(CaptionSink::new(config.caption_dir.clone()));
    let transcriber = spawn_worker(
        config.whisper_model_path.clone(),
        caption_sink.clone(),
        config.whisper_language.clone(),
        config.whisper_use_gpu,
        config.whisper_gpu_device,
    )?;
    let data = Arc::new(BotState::new(
        config.chunk_samples(),
        config.sample_rate,
        transcriber,
        speaker_updates.clone(),
        caption_sink,
        config.entry_sound_path.clone(),
    ));

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_VOICE_STATES;

    let songbird_config = SongbirdConfig::default()
        .crypto_mode(CryptoMode::XChaCha20Poly1305)
        .decode_mode(DecodeMode::Decode)
        .decode_sample_rate(sample_rate_from(config.sample_rate))
        .decode_channels(DecodeChannels::Mono);

    let framework = poise::Framework::builder()
        .options(FrameworkOptions {
            commands: vec![join(), leave(), ping()],
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            let data = Arc::clone(&data);
            let speaker_rx = Arc::clone(&speaker_rx);
            Box::pin(async move {
                tracing::info!("{} is connected", ready.user.name);
                if let Some(rx) = speaker_rx.lock().unwrap().take() {
                    tokio::spawn(run_presence_task(ctx.clone(), rx));
                }
                builtins::register_globally(ctx, &framework.options().commands).await?;
                tracing::info!("Invite URL: {}", build_invite_url(ready.user.id));
                Ok(data)
            })
        })
        .build();

    let mut client = DiscordClient::builder(&config.discord_token, intents)
        .framework(framework)
        .register_songbird_from_config(songbird_config)
        .await
        .context("creating Discord client")?;

    client.start().await.context("Discord client shutdown")?;

    Ok(())
}

#[poise::command(slash_command, guild_only)]
async fn join(
    ctx: BotContext<'_>,
    #[description = "Voice channel to join (defaults to your current channel)"] channel: Option<
        ChannelId,
    >,
    #[description = "Optional title for the generated notes"] title: Option<String>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    ctx.defer().await?;

    let session_title = title.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let target_channel = if let Some(id) = channel {
        id
    } else if let Some(channel_id) =
        current_voice_channel(ctx.serenity_context(), guild_id, ctx.author().id).await
    {
        channel_id
    } else {
        ctx.say("Join a voice channel first or supply a channel ID")
            .await?;
        return Ok(());
    };

    let Some(manager) = songbird::get(ctx.serenity_context()).await else {
        ctx.say("Voice client not initialised").await?;
        return Ok(());
    };
    let manager = manager.clone();

    let handler_lock = match manager.join(guild_id, target_channel).await {
        Ok(lock) => lock,
        Err(err) => {
            ctx.say(format!("Failed to join: {err:?}")).await?;
            return Ok(());
        }
    };

    let state = Arc::clone(ctx.data());
    let entry_sound_path = state.entry_sound_path.clone();
    if let Err(err) = play_entry_sound(&handler_lock, &entry_sound_path).await {
        tracing::warn!(?err, "Entry sound playback failed");
    }
    if let Err(err) = self_mute_call(&handler_lock).await {
        tracing::warn!(?err, "Failed to self-mute after joining");
    }

    if let Err(err) = attach_caption_pipeline(
        &handler_lock,
        CaptionPipelineConfig {
            guild_id,
            channel_id: target_channel,
            chunk_samples: state.chunk_samples,
            sample_rate: state.sample_rate,
            transcriber: state.transcriber.clone(),
            speaker_updates: Some(state.speaker_updates()),
            ctx: ctx.serenity_context().clone(),
        },
    )
    .await
    {
        ctx.say(format!("Failed to arm caption pipeline: {err:?}"))
            .await?;
    } else {
        state.track_call(guild_id, target_channel);
        if let Err(err) =
            state
                .caption_sink
                .start_session(guild_id, target_channel, session_title.clone())
        {
            tracing::error!(?err, "Failed to initialise caption session file");
            ctx.say("Joined, but failed to prepare the caption log on disk")
                .await?;
        } else {
            let mut response = format!("Listening in {}", target_channel.mention());
            if let Some(title) = session_title.as_ref() {
                response.push_str(&format!(" — notes titled \"{}\"", title));
            }
            ctx.say(response).await?;
        }
    }

    Ok(())
}

async fn play_entry_sound(call_lock: &CallLock, path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if !path.exists() {
        bail!("Entry sound missing at {}", path.display());
    }

    let input = SongbirdFile::new(path.to_path_buf());
    let handle = {
        let mut call = call_lock.lock().await;
        call.play_only_input(input.into())
    };
    if let Err(err) = handle.set_volume(ENTRY_SOUND_VOLUME) {
        tracing::warn!(?err, "Failed to set entry sound volume");
    }
    handle
        .make_playable_async()
        .await
        .map_err(|err| anyhow!("Entry track not playable: {err:?}"))?;

    let (tx, rx) = oneshot::channel();
    let notifier = TrackCompletionNotifier::new(tx);
    handle
        .add_event(Event::Track(TrackEvent::End), notifier)
        .map_err(|err| anyhow!("Failed to attach entry track observer: {err:?}"))?;

    timeout(ENTRY_SOUND_TIMEOUT, rx)
        .await
        .map_err(|_| anyhow!("Entry sound timed out after {:?}", ENTRY_SOUND_TIMEOUT))?
        .map_err(|_| anyhow!("Entry sound notifier dropped before completion"))?;

    Ok(())
}

async fn self_mute_call(call_lock: &CallLock) -> anyhow::Result<()> {
    let mut call = call_lock.lock().await;
    call.mute(true)
        .await
        .map_err(|err| anyhow!("Failed to self-mute: {err:?}"))?;
    Ok(())
}

struct TrackCompletionNotifier {
    tx: StdMutex<Option<oneshot::Sender<()>>>,
}

impl TrackCompletionNotifier {
    fn new(tx: oneshot::Sender<()>) -> Self {
        Self {
            tx: StdMutex::new(Some(tx)),
        }
    }
}

#[async_trait]
impl EventHandler for TrackCompletionNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if matches!(ctx, EventContext::Track(_))
            && let Some(tx) = self.tx.lock().unwrap().take()
        {
            let _ = tx.send(());
        }

        None
    }
}

#[poise::command(slash_command, guild_only)]
async fn leave(ctx: BotContext<'_>) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };

    ctx.defer().await?;

    let state = Arc::clone(ctx.data());
    let Some(manager) = songbird::get(ctx.serenity_context()).await else {
        ctx.say("Voice client not initialised").await?;
        return Ok(());
    };
    let manager = manager.clone();

    match manager.remove(guild_id).await {
        Ok(_) => {
            ctx.say("Left voice channel").await?;
            state.speaker_updates.clear();
            let transcript_path = state
                .take_call_channel(guild_id)
                .and_then(|channel| state.caption_sink.end_session(guild_id, channel));
            if let Some(file_path) = transcript_path
                && let Ok(contents) = std::fs::read_to_string(&file_path)
                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents)
            {
                let minified = serde_json::to_string(&json).unwrap_or(contents);
                use poise::{CreateReply, serenity_prelude::CreateAttachment};
                let file_name = file_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("transcript.json")
                    .to_string();
                ctx.send(
                    CreateReply::default()
                        .content("Transcript log")
                        .attachment(CreateAttachment::bytes(minified.into_bytes(), file_name)),
                )
                .await?;
            }
        }
        Err(err) => {
            ctx.say(format!("Failed to leave: {err}")).await?;
        }
    }

    Ok(())
}

#[poise::command(slash_command)]
async fn ping(ctx: BotContext<'_>) -> Result<(), Error> {
    ctx.say("Pong!").await?;
    Ok(())
}

async fn current_voice_channel(
    ctx: &serenity::Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Option<ChannelId> {
    let guild = ctx.cache.guild(guild_id)?;
    guild
        .voice_states
        .get(&user_id)
        .and_then(|state| state.channel_id)
}

fn sample_rate_from(value: u32) -> DecodeSampleRate {
    match value {
        8_000 => DecodeSampleRate::Hz8000,
        12_000 => DecodeSampleRate::Hz12000,
        16_000 => DecodeSampleRate::Hz16000,
        24_000 => DecodeSampleRate::Hz24000,
        48_000 => DecodeSampleRate::Hz48000,
        _ => DecodeSampleRate::Hz16000,
    }
}

async fn run_presence_task(ctx: serenity::Context, mut rx: SpeakerUpdateReceiver) {
    apply_presence(&ctx, current_speaker(&rx)).await;
    while rx.changed().await.is_ok() {
        apply_presence(&ctx, current_speaker(&rx)).await;
    }
    apply_presence(&ctx, None).await;
}

fn current_speaker(rx: &SpeakerUpdateReceiver) -> Option<UserId> {
    {
        let borrow = rx.borrow();
        *borrow
    }
}

async fn apply_presence(ctx: &serenity::Context, speaker: Option<UserId>) {
    let activity_label = match speaker {
        Some(user_id) => {
            let name = resolve_user_name(ctx, user_id).await;
            format!("Listening to: {name}")
        }
        None => "Listening for speakers".to_string(),
    };

    ctx.set_presence(
        Some(ActivityData::listening(activity_label)),
        OnlineStatus::Online,
    );
}

fn build_invite_url(bot_id: UserId) -> String {
    let permissions = invite_permissions().bits();
    format!(
        "https://discord.com/api/oauth2/authorize?client_id={}&permissions={permissions}&scope={INVITE_SCOPES}",
        bot_id.get()
    )
}

fn invite_permissions() -> Permissions {
    Permissions::VIEW_CHANNEL
        | Permissions::SEND_MESSAGES
        | Permissions::CONNECT
        | Permissions::SPEAK
        | Permissions::USE_VAD
}

async fn ensure_model_available(config: &BotConfig) -> anyhow::Result<()> {
    if config.whisper_model_path.exists() {
        return Ok(());
    }

    if let Ok(cli_path) = config.locate_whisper_cli() {
        if let Err(err) = attempt_cli_download(&cli_path, config).await {
            tracing::warn!("Whisper CLI download attempt failed: {err:?}");
        }

        if config.whisper_model_path.exists() {
            return Ok(());
        }
    }

    download_model_via_http(config).await?;

    if !config.whisper_model_path.exists() {
        bail!(
            "Failed to download Whisper model to {}",
            config.whisper_model_path.display()
        );
    }

    Ok(())
}

async fn attempt_cli_download(cli_path: &Path, config: &BotConfig) -> anyhow::Result<()> {
    let download_dir = config
        .whisper_model_path
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("creating model directory {}", download_dir.display()))?;

    let placeholder = download_dir.join(".whisper-download-placeholder.wav");
    fs::write(&placeholder, &[])
        .await
        .with_context(|| format!("creating placeholder {}", placeholder.display()))?;

    let status = Command::new(cli_path)
        .arg(&placeholder)
        .arg("--model")
        .arg(config.whisper_model_name())
        .arg("--model_dir")
        .arg(&download_dir)
        .arg("--output_dir")
        .arg(&download_dir)
        .arg("--device")
        .arg("cpu")
        .arg("--verbose")
        .arg("False")
        .status()
        .await
        .context("running whisper CLI for model download")?;

    let _ = fs::remove_file(&placeholder).await;

    if status.success() || config.whisper_model_path.exists() {
        return Ok(());
    }

    bail!("Whisper CLI exited with status {status}")
}

async fn download_model_via_http(config: &BotConfig) -> anyhow::Result<()> {
    let parent = config
        .whisper_model_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating model directory {}", parent.display()))?;

    let url = model_download_url(config.whisper_model_name());
    let tmp_path = config.whisper_model_path.with_extension("download");
    let client = HttpClient::new();

    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("downloading Whisper model from {url}"))?
        .error_for_status()
        .with_context(|| format!("unexpected response downloading Whisper model from {url}"))?;

    let mut file = fs::File::create(&tmp_path)
        .await
        .with_context(|| format!("creating {}", tmp_path.display()))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading bytes from {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing to {}", tmp_path.display()))?;
    }

    file.flush()
        .await
        .with_context(|| format!("flushing {}", tmp_path.display()))?;

    fs::rename(&tmp_path, &config.whisper_model_path)
        .await
        .with_context(|| {
            format!(
                "moving {} to {}",
                tmp_path.display(),
                config.whisper_model_path.display()
            )
        })?;

    Ok(())
}

fn model_download_url(model_name: &str) -> String {
    format!("{WHISPER_CPP_BASE_URL}/ggml-{model_name}.bin?download=1")
}
