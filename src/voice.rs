use std::{sync::Arc, time::Instant};

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
use tokio::sync::{Mutex, watch};
use tracing::{debug, error};

use crate::{
    discord_utils::resolve_user_name,
    transcription::{TranscriptionHandle, TranscriptionJob},
};

pub struct CaptionPipelineConfig {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub chunk_samples: usize,
    pub sample_rate: u32,
    pub transcriber: TranscriptionHandle,
    pub speaker_updates: Option<SpeakerUpdateSender>,
    pub ctx: Context,
}

pub async fn attach_caption_pipeline(
    call: &Arc<Mutex<Call>>,
    config: CaptionPipelineConfig,
) -> anyhow::Result<()> {
    let CaptionPipelineConfig {
        guild_id,
        channel_id,
        chunk_samples,
        sample_rate,
        transcriber,
        speaker_updates,
        ctx,
    } = config;

    let aggregator = Arc::new(AudioAggregator::new(
        guild_id,
        channel_id,
        chunk_samples,
        sample_rate,
        transcriber,
        speaker_updates,
        ctx,
    ));

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
    pending_audio: DashMap<u32, PendingAudio>,
    speaker_updates: Option<SpeakerUpdateSender>,
    current_speaker: Mutex<Option<UserId>>,
}

struct AudioBuffer {
    samples: Vec<i16>,
    speaker: UserId,
    last_activity: Instant,
}

impl AudioAggregator {
    fn new(
        guild_id: GuildId,
        channel_id: ChannelId,
        chunk_samples: usize,
        sample_rate: u32,
        transcriber: TranscriptionHandle,
        speaker_updates: Option<SpeakerUpdateSender>,
        ctx: Context,
    ) -> Self {
        Self {
            ctx,
            guild_id,
            channel_id,
            chunk_samples,
            sample_rate,
            transcriber,
            ssrc_map: DashMap::new(),
            buffers: DashMap::new(),
            pending_audio: DashMap::new(),
            speaker_updates,
            current_speaker: Mutex::new(None),
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
            self.promote_pending_audio(speaking.ssrc, serenity_id).await;
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
        } else {
            self.flush_stream(speaking.ssrc).await;
            if let Some(user_id) = self.resolve_speaking_user(speaking) {
                self.clear_current_speaker(user_id).await;
            }
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
        if let Some(user_id) = self.lookup_user(ssrc) {
            self.consume_samples(ssrc, user_id, samples).await;
        } else {
            debug!("[AUDIO] unknown ssrc {}, buffering audio", ssrc);
            self.buffer_pending(ssrc, samples);
        }
    }

    async fn consume_samples(&self, ssrc: u32, user_id: UserId, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }

        debug!(
            "[AUDIO] Received {} samples for user {:?}",
            samples.len(),
            user_id
        );
        let mut chunks = Vec::new();
        {
            let mut entry = self
                .buffers
                .entry(ssrc)
                .or_insert_with(|| AudioBuffer::new(user_id));

            entry.speaker = user_id;
            entry.samples.extend_from_slice(samples);
            entry.last_activity = Instant::now();

            while entry.samples.len() >= self.chunk_samples {
                let chunk: Vec<i16> = entry.samples.drain(..self.chunk_samples).collect();
                debug!(
                    "[AUDIO] Chunk ready for transcription: {} samples for user {:?}",
                    chunk.len(),
                    user_id
                );
                chunks.push(chunk);
            }
        }

        for chunk in chunks {
            self.dispatch_chunk(user_id, chunk).await;
        }
    }

    fn buffer_pending(&self, ssrc: u32, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }

        let mut entry = self
            .pending_audio
            .entry(ssrc)
            .or_insert_with(PendingAudio::new);
        entry.samples.extend_from_slice(samples);
    }

    async fn promote_pending_audio(&self, ssrc: u32, user_id: UserId) {
        if let Some((_, pending)) = self.pending_audio.remove(&ssrc) {
            let PendingAudio { samples, .. } = pending;
            if samples.is_empty() {
                return;
            }
            debug!(
                "[AUDIO] Promoting {} buffered samples for user {:?}",
                samples.len(),
                user_id
            );
            self.consume_samples(ssrc, user_id, &samples).await;
        }
    }

    async fn dispatch_chunk(&self, speaker: UserId, samples: Vec<i16>) {
        if samples.is_empty() {
            debug!("[TRANSCRIBE] Empty chunk for user {:?}, skipping", speaker);
            return;
        }

        debug!(
            "[TRANSCRIBE] Dispatching chunk for user {:?}: {} samples",
            speaker,
            samples.len()
        );
        self.set_current_speaker(speaker).await;

        let name = resolve_user_name(&self.ctx, speaker).await;

        let job = TranscriptionJob {
            channel_id: self.channel_id,
            guild_id: self.guild_id,
            speaker_id: speaker,
            speaker_name: name,
            pcm: samples,
            sample_rate: self.sample_rate,
            started_at: Utc::now(),
        };

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
            debug!(
                "[AUDIO] Flushing stream for ssrc {}: {} samples for user {:?}",
                ssrc,
                samples.len(),
                entry.speaker
            );
            self.dispatch_chunk(entry.speaker, samples).await;
        }
    }

    async fn flush_expired(&self, ssrc: u32) {
        if let Some(mut guard) = self.buffers.get_mut(&ssrc) {
            let should_flush =
                guard.last_activity.elapsed().as_secs_f32() > 1.0 && !guard.samples.is_empty();
            if should_flush {
                let speaker = guard.speaker;
                let samples = guard.samples.split_off(0);
                drop(guard);
                self.dispatch_chunk(speaker, samples).await;
            }
        }
    }

    fn lookup_user(&self, ssrc: u32) -> Option<UserId> {
        self.ssrc_map.get(&ssrc).map(|entry| *entry.value())
    }
}

impl AudioBuffer {
    fn new(speaker: UserId) -> Self {
        Self {
            samples: Vec::with_capacity(4096),
            speaker,
            last_activity: Instant::now(),
        }
    }
}

struct PendingAudio {
    samples: Vec<i16>,
}

impl PendingAudio {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(4096),
        }
    }
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
