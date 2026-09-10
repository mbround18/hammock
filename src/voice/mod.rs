use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
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
    transcription::{SpeakerLabel, SubmitOutcome, TranscriptionHandle, TranscriptionJob},
    utils::{resolve_user_name, resolve_user_name_cached},
};

pub mod roster;
pub mod segmenter;

use self::roster::VoiceRoster;

pub struct CaptionPipelineConfig {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub segmentation: segmenter::SegmentationConfig,
    pub sample_rate: u32,
    pub transcriber: TranscriptionHandle,
    pub speaker_updates: Option<SpeakerUpdateSender>,
    pub ctx: Context,
    pub caption_sink: Arc<CaptionSink>,
    pub roster: Arc<VoiceRoster>,
    pub metrics: Arc<crate::telemetry::AppMetrics>,
}

/// Attach the caption pipeline to a call.
///
/// Returns the aggregator so the caller can flush it when the bot leaves the
/// channel or shuts down (FR-007). Previously nothing held a reference, which
/// is why `/leave` could tear the call down with audio still buffered.
pub async fn attach_caption_pipeline(
    call: &Arc<Mutex<Call>>,
    config: CaptionPipelineConfig,
) -> anyhow::Result<Arc<AudioAggregator>> {
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
    Ok(aggregator)
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

pub struct AudioAggregator {
    ctx: Context,
    guild_id: GuildId,
    channel_id: ChannelId,
    segmentation: segmenter::SegmentationConfig,
    sample_rate: u32,
    transcriber: TranscriptionHandle,
    ssrc_map: DashMap<u32, UserId>,
    /// One segmenter per participant. Per-SSRC state is what makes FR-006
    /// structural: two speakers share nothing, so one's pauses cannot end the
    /// other's utterance.
    segmenters: DashMap<u32, segmenter::Segmenter>,
    placeholder_labels: DashMap<u32, String>,
    speaker_updates: Option<SpeakerUpdateSender>,
    current_speaker: Mutex<Option<UserId>>,
    caption_sink: Arc<CaptionSink>,
    roster: Arc<VoiceRoster>,
    metrics: Arc<crate::telemetry::AppMetrics>,
}

impl AudioAggregator {
    fn new(config: CaptionPipelineConfig) -> Self {
        let CaptionPipelineConfig {
            guild_id,
            channel_id,
            segmentation,
            sample_rate,
            transcriber,
            speaker_updates,
            ctx,
            caption_sink,
            roster,
            metrics,
        } = config;
        Self {
            ctx,
            guild_id,
            channel_id,
            segmentation,
            sample_rate,
            transcriber,
            ssrc_map: DashMap::new(),
            segmenters: DashMap::new(),
            placeholder_labels: DashMap::new(),
            speaker_updates,
            current_speaker: Mutex::new(None),
            caption_sink,
            roster,
            metrics,
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
        let now = Instant::now();

        // Both halves of the tick feed the segmenter. The silent set used to be
        // consulted only for an elapsed-time flush, but it *is* the boundary
        // signal: Discord clients run their own voice-activity detection and
        // stop transmitting when nobody is talking, so an SSRC's absence from
        // `speaking` already means the sending client thinks this person
        // stopped (research R1).
        for (ssrc, data) in &tick.speaking {
            let samples = data.decoded_voice.as_deref().unwrap_or(&[]);
            self.advance(*ssrc, true, samples, now).await;
        }

        for ssrc in &tick.silent {
            self.advance(*ssrc, false, &[], now).await;
        }

        None
    }

    /// Advance one participant's segmenter by a tick, dispatching whatever
    /// utterance it closes.
    async fn advance(&self, ssrc: u32, speaking: bool, samples: &[i16], now: Instant) {
        // Only open a segmenter for a participant we have actually heard from.
        // A silent tick for a stranger is not worth an allocation, and every
        // SSRC in the channel produces one every 20 ms.
        if !speaking && !self.segmenters.contains_key(&ssrc) {
            return;
        }

        let identity = self.resolve_identity(ssrc, None).await;

        let closed = {
            let mut segmenter = self
                .segmenters
                .entry(ssrc)
                .or_insert_with(|| segmenter::Segmenter::new(self.segmentation, self.sample_rate));
            segmenter.on_tick(speaking, samples, &identity, now)
        };

        if let Some(utterance) = closed {
            self.dispatch_utterance(utterance).await;
        }
    }

    async fn dispatch_utterance(&self, utterance: segmenter::Utterance) {
        if utterance.samples.is_empty() {
            return;
        }

        let audio_len = utterance.audio_duration(self.sample_rate);
        debug!(
            "[TRANSCRIBE] Utterance closed: {} samples ({:.2}s audio, {:.2}s speech)",
            utterance.samples.len(),
            audio_len.as_secs_f32(),
            utterance.speech_duration().as_secs_f32(),
        );
        // FR-011. The shape of this distribution is how an operator sees that
        // segmentation is misconfigured — a median collapsing toward the
        // minimum means the silence threshold is too low.
        self.metrics.record_utterance_length(audio_len);

        let (speaker_id, speaker) = match utterance.speaker.clone() {
            SpeakerIdentity::Known(user_id) => {
                self.set_current_speaker(user_id).await;
                // A cache hit costs nothing and can stay here. A miss would be
                // an HTTP round trip, and this runs inside the 20 ms voice tick
                // handler, where Constitution Principle I forbids awaiting a
                // network call — so a miss is deferred to the dispatcher
                // instead, which resolves it off this path.
                let label = match resolve_user_name_cached(&self.ctx, user_id) {
                    Some(name) => SpeakerLabel::Resolved(name),
                    None => SpeakerLabel::Deferred {
                        user_id,
                        ctx: self.ctx.clone(),
                    },
                };
                (Some(user_id), label)
            }
            SpeakerIdentity::Placeholder { label } => (None, SpeakerLabel::Resolved(label)),
        };

        let job = TranscriptionJob {
            channel_id: self.channel_id,
            guild_id: self.guild_id,
            speaker_id,
            speaker,
            pcm: utterance.samples,
            sample_rate: self.sample_rate,
            // FR-014: when the speech began, not when the segment closed or
            // when transcription happened to start. The previous code stamped
            // `Utc::now()` here, which was never a record of when anyone spoke.
            started_at: utterance.started_at,
            queued_at: None,
        };

        if let Some(user_id) = job.speaker_id {
            self.roster.note_spoke(user_id).await;
        }

        // Not awaited, and that is the point: submission is synchronous, so a
        // full transcription queue can no longer stall audio reception for
        // every speaker in this channel. An overloaded transcriber now costs
        // one dropped utterance, counted and logged at the submission site,
        // instead of corrupting everyone's stream.
        match self.transcriber.submit(job) {
            SubmitOutcome::Queued => debug!(
                "[TRANSCRIBE] Transcription job queued for user {:?}",
                speaker_id
            ),
            // Both already logged with full context inside `submit`; logging
            // again here would double-count them in an operator's eyes.
            SubmitOutcome::Discarded | SubmitOutcome::Closed => {}
        }
    }

    /// Close and dispatch a participant's open utterance, whatever its length.
    ///
    /// FR-007's three cases: the speaker disconnected, the bot is leaving, or
    /// the process is shutting down. Audio buffered at those moments used to be
    /// dropped silently — `/leave` in particular tore the call down with
    /// nothing flushing first.
    async fn flush_stream(&self, ssrc: u32) {
        let closed = self
            .segmenters
            .remove(&ssrc)
            .and_then(|(_, mut segmenter)| segmenter.flush());

        if let Some(utterance) = closed {
            debug!(
                "[AUDIO] Flushing stream for ssrc {}: {} samples",
                ssrc,
                utterance.samples.len()
            );
            self.dispatch_utterance(utterance).await;
        }
    }

    /// Flush every participant. Called when the bot leaves a channel or shuts
    /// down (FR-007, SC-007).
    pub async fn flush_all(&self) {
        let ssrcs: Vec<u32> = self.segmenters.iter().map(|entry| *entry.key()).collect();
        for ssrc in ssrcs {
            self.flush_stream(ssrc).await;
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

#[derive(Clone, Debug)]
pub enum SpeakerIdentity {
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
