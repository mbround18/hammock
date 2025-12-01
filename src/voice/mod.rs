use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use serenity::{
    model::id::{ChannelId, GuildId, UserId},
    prelude::Context,
};
use songbird::{
    Call,
    events::{
        CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, context_data::VoiceTick,
    },
    model::{
        id::UserId as VoiceUserId,
        payload::{ClientDisconnect, Speaking},
    },
};
use tokio::{
    sync::{Mutex, watch},
    task,
};
use tracing::{debug, error};

use crate::{
    captions::CaptionSink,
    transcription::{TranscriptionHandle, TranscriptionJob},
    utils::resolve_user_name,
};

pub mod roster;

use self::roster::VoiceRoster;

pub struct CaptionPipelineConfig {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub chunk_samples: usize,
    pub sample_rate: u32,
    pub transcriber: TranscriptionHandle,
    pub speaker_updates: Option<SpeakerUpdateSender>,
    pub ctx: Context,
    pub caption_sink: Arc<CaptionSink>,
    pub silence_flush: Duration,
    pub roster: Arc<VoiceRoster>,
}

pub async fn attach_caption_pipeline(
    call: &Arc<Mutex<Call>>,
    config: CaptionPipelineConfig,
) -> anyhow::Result<()> {
    let guild_id = config.guild_id;
    let channel_id = config.channel_id;

    let aggregator = Arc::new(AudioAggregator::new(config));

    let handler = CaptionReceiver::new(Arc::clone(&aggregator));

    let mut call_guard = call.lock().await;
    debug!(
        "[DIAG] attach_caption_pipeline: Adding global events for guild {:?} channel {:?}",
        guild_id, channel_id
    );
    call_guard.add_global_event(Event::Core(CoreEvent::SpeakingStateUpdate), handler.clone());
    call_guard.add_global_event(Event::Core(CoreEvent::VoiceTick), handler.clone());
    call_guard.add_global_event(Event::Core(CoreEvent::ClientDisconnect), handler);
    // Log initial ssrc_map state
    let map_snapshot: Vec<(u32, UserId)> = aggregator
        .ssrc_map
        .iter()
        .map(|e| (*e.key(), *e.value()))
        .collect();
    debug!("[DIAG] Initial SSRC map: {:?}", map_snapshot);
    Ok(())
}

#[derive(Clone)]
struct CaptionReceiver {
    aggregator: Arc<AudioAggregator>,
}

impl CaptionReceiver {
    fn new(aggregator: Arc<AudioAggregator>) -> Self {
        Self { aggregator }
    }
}

#[async_trait]
impl VoiceEventHandler for CaptionReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(update) => self.aggregator.on_speaking(update).await,
            EventContext::VoiceTick(tick) => self.aggregator.on_voice_tick(tick).await,
            EventContext::ClientDisconnect(disconnect) => {
                self.aggregator.on_disconnect(disconnect).await
            }
            _ => None,
        }
    }
}

struct AudioAggregator {
    ctx: Context,
    guild_id: GuildId,
    channel_id: ChannelId,
    chunk_samples: usize,
    sample_rate: u32,
    transcriber: TranscriptionHandle,
    ssrc_map: DashMap<u32, UserId>,
    buffers: DashMap<u32, AudioBuffer>,
    placeholder_labels: DashMap<u32, String>,
    speaker_updates: Option<SpeakerUpdateSender>,
    current_speaker: Mutex<Option<UserId>>,
    caption_sink: Arc<CaptionSink>,
    silence_flush: Duration,
    roster: Arc<VoiceRoster>,
}

struct AudioBuffer {
    samples: Vec<i16>,
    speaker: SpeakerIdentity,
    last_activity: Instant,
}

impl AudioAggregator {
    fn new(config: CaptionPipelineConfig) -> Self {
        let CaptionPipelineConfig {
            guild_id,
            channel_id,
            chunk_samples,
            sample_rate,
            transcriber,
            speaker_updates,
            ctx,
            caption_sink,
            silence_flush,
            roster,
        } = config;
        Self {
            ctx,
            guild_id,
            channel_id,
            chunk_samples,
            sample_rate,
            transcriber,
            ssrc_map: DashMap::new(),
            buffers: DashMap::new(),
            placeholder_labels: DashMap::new(),
            speaker_updates,
            current_speaker: Mutex::new(None),
            caption_sink,
            silence_flush,
            roster,
        }
    }

    async fn on_speaking(&self, speaking: &Speaking) -> Option<Event> {
        debug!(
            "[DIAG] Speaking event: ssrc={}, user_id={:?}, speaking_flags={:?}",
            speaking.ssrc, speaking.user_id, speaking.speaking
        );
        if let Some(user_id) = speaking.user_id {
            let serenity_id = to_serenity_user_id(user_id);
            self.ssrc_map.insert(speaking.ssrc, serenity_id);
            debug!(
                "[DIAG] on_speaking: mapped ssrc {} to user {:?}",
                speaking.ssrc, serenity_id
            );
            if let Some((_, label)) = self.placeholder_labels.remove(&speaking.ssrc) {
                self.relabel_placeholder_entries(label, serenity_id).await;
            }
        } else {
            debug!("[DIAG] on_speaking: no user_id for ssrc {}", speaking.ssrc);
        }

        // Log current ssrc_map
        let map_snapshot: Vec<(u32, UserId)> = self
            .ssrc_map
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        debug!("[DIAG] Current SSRC map: {:?}", map_snapshot);

        if speaking.speaking.microphone() {
            if let Some(user_id) = self.resolve_speaking_user(speaking) {
                self.set_current_speaker(user_id).await;
            }
        } else if let Some(user_id) = self.resolve_speaking_user(speaking) {
            self.clear_current_speaker(user_id).await;
        }

        None
    }

    async fn on_disconnect(&self, disconnect: &ClientDisconnect) -> Option<Event> {
        let user_id = to_serenity_user_id(disconnect.user_id);
        let ssrcs: Vec<u32> = self
            .ssrc_map
            .iter()
            .filter_map(|entry| {
                if *entry.value() == user_id {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect();

        for ssrc in ssrcs {
            self.flush_stream(ssrc).await;
            self.ssrc_map.remove(&ssrc);
            self.clear_current_speaker(user_id).await;
        }

        None
    }

    async fn on_voice_tick(&self, tick: &VoiceTick) -> Option<Event> {
        for (ssrc, data) in &tick.speaking {
            if let Some(decoded) = data.decoded_voice.as_ref() {
                self.push_samples(*ssrc, decoded).await;
            }
        }

        for ssrc in &tick.silent {
            self.flush_expired(*ssrc).await;
        }

        None
    }

    async fn push_samples(&self, ssrc: u32, samples: &[i16]) {
        let identity = self.resolve_identity(ssrc, None).await;
        self.consume_samples(ssrc, identity, samples).await;
    }

    async fn consume_samples(&self, ssrc: u32, identity: SpeakerIdentity, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }

        debug!(
            "[AUDIO] Received {} samples for ssrc {}",
            samples.len(),
            ssrc
        );
        let mut chunks = Vec::new();
        {
            let mut entry = self
                .buffers
                .entry(ssrc)
                .or_insert_with(|| AudioBuffer::new(identity.clone()));

            entry.speaker = identity.clone();
            entry.samples.extend_from_slice(samples);
            entry.last_activity = Instant::now();

            while entry.samples.len() >= self.chunk_samples {
                let chunk: Vec<i16> = entry.samples.drain(..self.chunk_samples).collect();
                debug!(
                    "[AUDIO] Chunk ready for transcription: {} samples for ssrc {}",
                    chunk.len(),
                    ssrc
                );
                chunks.push(chunk);
            }
        }

        for chunk in chunks {
            self.dispatch_chunk(identity.clone(), chunk).await;
        }
    }

    async fn dispatch_chunk(&self, identity: SpeakerIdentity, samples: Vec<i16>) {
        if samples.is_empty() {
            debug!("[TRANSCRIBE] Empty chunk, skipping");
            return;
        }

        debug!("[TRANSCRIBE] Dispatching chunk: {} samples", samples.len());

        let (speaker_id, speaker_name) = match identity.clone() {
            SpeakerIdentity::Known(user_id) => {
                self.set_current_speaker(user_id).await;
                let name = resolve_user_name(&self.ctx, user_id).await;
                (Some(user_id), name)
            }
            SpeakerIdentity::Placeholder { label } => (None, label),
        };

        let job = TranscriptionJob {
            channel_id: self.channel_id,
            guild_id: self.guild_id,
            speaker_id,
            speaker_name,
            pcm: samples,
            sample_rate: self.sample_rate,
            started_at: Utc::now(),
        };

        if let Some(user_id) = job.speaker_id {
            self.roster.note_spoke(user_id).await;
        }

        let speaker_id = job.speaker_id;
        if let Err(err) = self.transcriber.submit(job).await {
            error!("[TRANSCRIBE] failed to queue transcription: {err:?}");
        } else {
            debug!(
                "[TRANSCRIBE] Transcription job queued for user {:?}",
                speaker_id
            );
        }
    }

    async fn flush_stream(&self, ssrc: u32) {
        if let Some((_, mut entry)) = self.buffers.remove(&ssrc)
            && !entry.samples.is_empty()
        {
            let samples = entry.samples.split_off(0);
            let identity = self
                .resolve_identity(ssrc, Some(entry.speaker.clone()))
                .await;
            debug!(
                "[AUDIO] Flushing stream for ssrc {}: {} samples",
                ssrc,
                samples.len(),
            );
            self.dispatch_chunk(identity, samples).await;
        }
    }

    async fn flush_expired(&self, ssrc: u32) {
        if let Some(mut guard) = self.buffers.get_mut(&ssrc) {
            let should_flush =
                guard.last_activity.elapsed() > self.silence_flush && !guard.samples.is_empty();
            if should_flush {
                let samples = guard.samples.split_off(0);
                let speaker = guard.speaker.clone();
                drop(guard);
                let identity = self.resolve_identity(ssrc, Some(speaker)).await;
                self.dispatch_chunk(identity, samples).await;
            }
        }
    }

    fn lookup_user(&self, ssrc: u32) -> Option<UserId> {
        self.ssrc_map.get(&ssrc).map(|entry| *entry.value())
    }

    fn placeholder_label(&self, ssrc: u32) -> String {
        if let Some(existing) = self.placeholder_labels.get(&ssrc) {
            existing.clone()
        } else {
            let label = format!("Speaker {ssrc}");
            self.placeholder_labels.insert(ssrc, label.clone());
            label
        }
    }

    async fn resolve_identity(
        &self,
        ssrc: u32,
        existing: Option<SpeakerIdentity>,
    ) -> SpeakerIdentity {
        if let Some(SpeakerIdentity::Known(user_id)) = existing.clone() {
            return SpeakerIdentity::Known(user_id);
        }

        if let Some(user_id) = self.lookup_user(ssrc) {
            return SpeakerIdentity::Known(user_id);
        }

        if let Some(user_id) = self.roster.guess_speaker(self.channel_id).await {
            self.ssrc_map.insert(ssrc, user_id);
            return SpeakerIdentity::Known(user_id);
        }

        match existing {
            Some(SpeakerIdentity::Placeholder { label }) => SpeakerIdentity::Placeholder { label },
            _ => SpeakerIdentity::Placeholder {
                label: self.placeholder_label(ssrc),
            },
        }
    }

    async fn relabel_placeholder_entries(&self, placeholder: String, user_id: UserId) {
        let new_name = resolve_user_name(&self.ctx, user_id).await;
        let sink = Arc::clone(&self.caption_sink);
        let guild_id = self.guild_id;
        let channel_id = self.channel_id;

        let placeholder_for_logs = placeholder.clone();
        match task::spawn_blocking(move || {
            sink.relabel_placeholder(guild_id, channel_id, &placeholder, user_id, &new_name)
        })
        .await
        {
            Ok(Ok(true)) => debug!(
                "[CAPTION] Relabeled placeholder '{}' as user {:?}",
                placeholder_for_logs, user_id
            ),
            Ok(Ok(false)) => debug!(
                "[CAPTION] No entries needed relabel for placeholder '{}'",
                placeholder_for_logs
            ),
            Ok(Err(err)) => error!(
                "[CAPTION] Failed relabeling placeholder '{}': {err:?}",
                placeholder_for_logs
            ),
            Err(err) => error!(
                "[CAPTION] Relabeling task join error for placeholder '{}': {err}",
                placeholder_for_logs
            ),
        }
    }
}

impl AudioBuffer {
    fn new(speaker: SpeakerIdentity) -> Self {
        Self {
            samples: Vec::with_capacity(4096),
            speaker,
            last_activity: Instant::now(),
        }
    }
}

#[derive(Clone, Debug)]
enum SpeakerIdentity {
    Known(UserId),
    Placeholder { label: String },
}

fn to_serenity_user_id(id: VoiceUserId) -> UserId {
    UserId::new(id.0)
}

impl AudioAggregator {
    fn resolve_speaking_user(&self, speaking: &Speaking) -> Option<UserId> {
        speaking
            .user_id
            .map(to_serenity_user_id)
            .or_else(|| self.lookup_user(speaking.ssrc))
    }

    async fn set_current_speaker(&self, speaker: UserId) {
        if let Some(notifier) = &self.speaker_updates {
            let mut guard = self.current_speaker.lock().await;
            if guard.as_ref() != Some(&speaker) {
                *guard = Some(speaker);
                notifier.notify(Some(speaker));
            }
        }
    }

    async fn clear_current_speaker(&self, speaker: UserId) {
        if let Some(notifier) = &self.speaker_updates {
            let mut guard = self.current_speaker.lock().await;
            if guard.as_ref() == Some(&speaker) {
                *guard = None;
                notifier.notify(None);
            }
        }
    }
}

#[derive(Clone)]
pub struct SpeakerUpdateSender {
    tx: watch::Sender<Option<UserId>>,
}

pub type SpeakerUpdateReceiver = watch::Receiver<Option<UserId>>;

pub fn speaker_update_channel() -> (SpeakerUpdateSender, SpeakerUpdateReceiver) {
    let (tx, rx) = watch::channel(None);
    (SpeakerUpdateSender { tx }, rx)
}

impl SpeakerUpdateSender {
    pub fn notify(&self, speaker: Option<UserId>) {
        let _ = self.tx.send(speaker);
    }

    pub fn clear(&self) {
        self.notify(None);
    }
}
