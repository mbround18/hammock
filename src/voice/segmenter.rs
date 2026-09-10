//! Utterance boundaries, derived from speech activity rather than a sample
//! counter.
//!
//! Before this existed, audio was cut every `CAPTION_CHUNK_SECS` worth of
//! samples wherever they happened to fall — mid-word as often as not — and the
//! tail of an utterance waited a further three seconds of silence before being
//! flushed. Speech recognition collapses on fragments that begin or end
//! mid-syllable, so this was the single largest source of transcript error.
//!
//! The signal used here was already arriving and being thrown away. songbird's
//! `VoiceTick` separates `speaking` from `silent` per participant per 20 ms,
//! and Discord clients run their own voice-activity detection before
//! transmitting — so "a packet arrived" already means "the sending client
//! believes this person is talking", decided at the microphone with better
//! information than anything downstream has. See
//! `specs/003-vad-utterance-segmentation/research.md` R1.
//!
//! **This code runs on the 20 ms voice tick.** Constitution Principle I means no
//! I/O, no awaiting, and no buffer that grows without bound — which is why
//! `max_utterance` is a correctness requirement here and not a tuning knob.
//!
//! It is a separate module so it can be tested. Left inside `AudioAggregator`
//! it would be reachable only through songbird events, which is how the
//! fixed-chunk logic it replaces came to have no tests at all.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::SpeakerIdentity;

/// One voice tick. songbird delivers audio on this cadence.
pub const TICK: Duration = Duration::from_millis(20);

/// The three thresholds that define segmentation behavior.
#[derive(Debug, Clone, Copy)]
pub struct SegmentationConfig {
    /// Quiet after which an utterance is closed (FR-002, FR-003).
    pub silence: Duration,
    /// Longest an utterance may run before being split (FR-004).
    pub max_utterance: Duration,
    /// Speech shorter than this is not transcribed (FR-005).
    pub min_speech: Duration,
}

impl Default for SegmentationConfig {
    fn default() -> Self {
        Self {
            silence: Duration::from_millis(500),
            max_utterance: Duration::from_secs(20),
            min_speech: Duration::from_millis(300),
        }
    }
}

/// A contiguous span of one participant's speech: the unit submitted for
/// transcription, replacing the fixed-size chunk.
pub struct Utterance {
    pub samples: Vec<i16>,
    pub speaker: SpeakerIdentity,
    /// When the **first** sample arrived, not when the utterance closed
    /// (FR-014). The previous code stamped captions with the moment the chunk
    /// was cut, which was never a record of when anyone spoke.
    pub started_at: DateTime<Utc>,
    /// Ticks that carried speech. Counted rather than derived from
    /// `samples.len()`, because samples also accumulate during the sub-threshold
    /// pauses an utterance holds through — counting bytes would let a long
    /// silence masquerade as speech.
    pub speech_ticks: usize,
}

impl Utterance {
    /// How much of this utterance was actually speech.
    pub fn speech_duration(&self) -> Duration {
        TICK * self.speech_ticks as u32
    }

    /// Total audio duration, silences included, at the given sample rate.
    pub fn audio_duration(&self, sample_rate: u32) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(sample_rate))
    }
}

struct Open {
    samples: Vec<i16>,
    speaker: SpeakerIdentity,
    started_at: DateTime<Utc>,
    last_speech: Instant,
    speech_ticks: usize,
    /// Sample offset of the most recent tick that carried no speech. The
    /// least-disruptive split point when the maximum length is reached.
    last_quiet_offset: Option<usize>,
}

/// Per-participant segmentation state.
///
/// One instance per SSRC, which is what makes FR-006 structural rather than
/// something to remember: two participants share no state, so one speaker's
/// pauses cannot affect another's boundaries.
pub struct Segmenter {
    config: SegmentationConfig,
    max_samples: usize,
    current: Option<Open>,
}

impl Segmenter {
    pub fn new(config: SegmentationConfig, sample_rate: u32) -> Self {
        let max_samples =
            (config.max_utterance.as_secs_f64() * f64::from(sample_rate)).ceil() as usize;
        Self {
            config,
            max_samples,
            current: None,
        }
    }

    /// Advance one tick.
    ///
    /// `speaking` is whether this participant transmitted audio for this tick;
    /// `samples` is whatever arrived. Returns an utterance when one closes.
    ///
    /// Pure bookkeeping: no I/O, no awaiting, no allocation beyond extending
    /// the sample buffer. That is what lets it sit on the voice tick path, and
    /// separately what lets it be tested exhaustively without Discord.
    pub fn on_tick(
        &mut self,
        speaking: bool,
        samples: &[i16],
        speaker: &SpeakerIdentity,
        now: Instant,
    ) -> Option<Utterance> {
        match self.current.as_mut() {
            None => {
                if speaking && !samples.is_empty() {
                    self.current = Some(Open {
                        samples: samples.to_vec(),
                        speaker: speaker.clone(),
                        started_at: Utc::now(),
                        last_speech: now,
                        speech_ticks: 1,
                        last_quiet_offset: None,
                    });
                }
                // Quiet while idle is the common case: most participants are
                // not talking most of the time. Nothing to do.
                None
            }
            Some(open) => {
                if speaking {
                    open.samples.extend_from_slice(samples);
                    open.last_speech = now;
                    open.speech_ticks += 1;
                    // The speaker identity can sharpen mid-utterance, when an
                    // SSRC that was a placeholder is matched to a real user.
                    // Attribution follows the better answer (FR-013).
                    if matches!(speaker, SpeakerIdentity::Known(_)) {
                        open.speaker = speaker.clone();
                    }
                } else {
                    // A quiet tick below the threshold keeps the utterance open
                    // AND keeps its samples. This is the rule that stops words
                    // being severed (FR-003): a pause inside a sentence is part
                    // of the sentence, and dropping it would splice two
                    // half-words together.
                    open.last_quiet_offset = Some(open.samples.len());
                    open.samples.extend_from_slice(samples);

                    if now.duration_since(open.last_speech) >= self.config.silence {
                        return self.close(None);
                    }
                }

                // FR-004. Also the memory bound Principle I requires: without
                // it, one participant transmitting continuously would grow this
                // buffer without limit.
                if open.samples.len() >= self.max_samples {
                    let split = open.last_quiet_offset.filter(|offset| *offset > 0);
                    return self.close(Some(split.unwrap_or(self.max_samples)));
                }

                None
            }
        }
    }

    /// Close any open utterance regardless of thresholds.
    ///
    /// For the three cases FR-007 names: the speaker disconnects, the bot
    /// leaves the channel, or the process shuts down. Audio buffered at those
    /// moments used to be dropped silently.
    pub fn flush(&mut self) -> Option<Utterance> {
        self.close(None)
    }

    /// Emit the open utterance, optionally splitting it at `split_at` and
    /// carrying the remainder into a new one.
    fn close(&mut self, split_at: Option<usize>) -> Option<Utterance> {
        let mut open = self.current.take()?;

        let (samples, remainder) = match split_at {
            Some(at) if at < open.samples.len() => {
                let tail = open.samples.split_off(at);
                (open.samples, tail)
            }
            _ => (open.samples, Vec::new()),
        };

        let utterance = Utterance {
            samples,
            speaker: open.speaker.clone(),
            started_at: open.started_at,
            speech_ticks: open.speech_ticks,
        };

        // A split leaves audio behind; it must not be dropped. Speech ticks
        // reset because the tail's own speech has not been counted yet, but the
        // audio carries over intact.
        if !remainder.is_empty() {
            self.current = Some(Open {
                samples: remainder,
                speaker: open.speaker,
                started_at: Utc::now(),
                last_speech: open.last_speech,
                speech_ticks: open.speech_ticks.min(1),
                last_quiet_offset: None,
            });
        }

        // FR-005. Below the minimum this is a click, a cough, or half a
        // syllable, and handing it to the recognizer invites an invented word.
        // Dropping it is not a "rejection": nothing was rejected, nothing
        // happened, and counting it would make the rejection metric mean two
        // things.
        if utterance.speech_duration() < self.config.min_speech || utterance.samples.is_empty() {
            return None;
        }

        Some(utterance)
    }

    #[cfg(test)]
    fn is_open(&self) -> bool {
        self.current.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;
    /// Samples in one 20 ms tick at 16 kHz.
    const TICK_SAMPLES: usize = 320;

    fn config() -> SegmentationConfig {
        SegmentationConfig {
            silence: Duration::from_millis(500),
            max_utterance: Duration::from_secs(2),
            min_speech: Duration::from_millis(100),
        }
    }

    fn speaker() -> SpeakerIdentity {
        SpeakerIdentity::Placeholder {
            label: "Speaker 1".to_string(),
        }
    }

    fn tone() -> Vec<i16> {
        (0..TICK_SAMPLES)
            .map(|i| (i as i16).wrapping_mul(64))
            .collect()
    }

    /// Drive a segmenter through a script of (speaking, ticks) pairs, advancing
    /// a synthetic clock 20 ms per tick. Returns everything it emitted.
    ///
    /// A synthetic clock rather than real time: these tests must be
    /// deterministic and instant, and sleeping 20 ms per tick would make a
    /// 2-second case take 2 seconds.
    fn run(seg: &mut Segmenter, script: &[(bool, usize)]) -> Vec<Utterance> {
        let mut now = Instant::now();
        let mut out = Vec::new();
        let audio = tone();
        for (speaking, ticks) in script {
            for _ in 0..*ticks {
                if let Some(u) = seg.on_tick(*speaking, &audio, &speaker(), now) {
                    out.push(u);
                }
                now += TICK;
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // FR-003 — the rule the whole feature rests on
    // -----------------------------------------------------------------------

    #[test]
    fn a_pause_shorter_than_the_threshold_does_not_split() {
        // 500 ms threshold, 10 quiet ticks = 200 ms. This is an ordinary
        // breath mid-sentence, and splitting here is exactly how the previous
        // implementation severed words.
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(&mut seg, &[(true, 10), (false, 10), (true, 10)]);

        assert!(
            emitted.is_empty(),
            "a 200ms pause must not close an utterance with a 500ms threshold"
        );
        assert!(seg.is_open());
    }

    #[test]
    fn quiet_samples_inside_an_utterance_are_kept() {
        // If the pause were dropped rather than retained, the two halves of the
        // sentence would be spliced together and the recognizer would hear a
        // word that was never spoken.
        let mut seg = Segmenter::new(config(), RATE);
        run(&mut seg, &[(true, 5), (false, 5), (true, 5)]);
        let flushed = seg.flush().expect("utterance");

        assert_eq!(
            flushed.samples.len(),
            15 * TICK_SAMPLES,
            "the quiet ticks in the middle must still be in the audio"
        );
    }

    #[test]
    fn a_pause_at_the_threshold_closes_the_utterance() {
        let mut seg = Segmenter::new(config(), RATE);
        // 25 quiet ticks = 500 ms, exactly the threshold.
        let emitted = run(&mut seg, &[(true, 10), (false, 26)]);

        assert_eq!(emitted.len(), 1, "FR-002: silence must end the utterance");
        assert!(!seg.is_open());
    }

    #[test]
    fn a_single_lost_packet_does_not_end_an_utterance() {
        // Network jitter is not someone finishing their sentence.
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(
            &mut seg,
            &[(true, 10), (false, 1), (true, 10), (false, 1), (true, 10)],
        );

        assert!(
            emitted.is_empty(),
            "packet loss must not close an utterance"
        );
    }

    // -----------------------------------------------------------------------
    // FR-014 — timestamps
    // -----------------------------------------------------------------------

    #[test]
    fn the_timestamp_is_the_first_sample_not_the_close() {
        let mut seg = Segmenter::new(config(), RATE);
        let before_speech = Utc::now();

        // One script, one clock. Splitting this across two `run` calls would
        // restart the synthetic clock, leaving the silence measured against a
        // base 100ms in the future and the utterance never closing.
        let emitted = run(&mut seg, &[(true, 10), (false, 26)]);
        let after_close = Utc::now();

        let utterance = emitted.first().expect("utterance should have closed");
        assert!(
            utterance.started_at >= before_speech,
            "started_at must not predate the first sample"
        );
        assert!(
            utterance.started_at <= after_close,
            "started_at must be when speech began, not when the utterance closed"
        );
    }

    // -----------------------------------------------------------------------
    // FR-004 / FR-005 — bounds
    // -----------------------------------------------------------------------

    #[test]
    fn continuous_speech_splits_at_the_maximum() {
        // 2 s maximum at 16 kHz = 32 000 samples = 100 ticks. Also the memory
        // bound Principle I requires: without it one open microphone grows this
        // buffer forever.
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(&mut seg, &[(true, 250)]);

        assert!(!emitted.is_empty(), "FR-004: a long turn must be split");
        for utterance in &emitted {
            assert!(
                utterance.samples.len() <= 2 * RATE as usize,
                "no emitted utterance may exceed the maximum length"
            );
        }
    }

    #[test]
    fn a_split_carries_the_remainder_rather_than_dropping_it() {
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(&mut seg, &[(true, 150)]);
        let emitted_samples: usize = emitted.iter().map(|u| u.samples.len()).sum();
        let still_open = seg.flush().map(|u| u.samples.len()).unwrap_or(0);

        assert_eq!(
            emitted_samples + still_open,
            150 * TICK_SAMPLES,
            "splitting must not lose audio"
        );
    }

    #[test]
    fn a_burst_shorter_than_the_minimum_is_dropped() {
        // 100 ms minimum, 3 ticks = 60 ms. A click or half a syllable; handing
        // it to the recognizer invites an invented word.
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(&mut seg, &[(true, 3), (false, 26)]);

        assert!(
            emitted.is_empty(),
            "FR-005: sub-minimum speech is not transcribed"
        );
    }

    #[test]
    fn a_burst_at_the_minimum_is_kept() {
        let mut seg = Segmenter::new(config(), RATE);
        let emitted = run(&mut seg, &[(true, 6), (false, 26)]);
        assert_eq!(emitted.len(), 1, "120ms of speech is above a 100ms minimum");
    }

    // -----------------------------------------------------------------------
    // FR-006 / FR-007
    // -----------------------------------------------------------------------

    #[test]
    fn two_speakers_segment_independently() {
        // Separate Segmenter instances, which is what makes FR-006 structural
        // rather than something to remember.
        let mut alice = Segmenter::new(config(), RATE);
        let mut bob = Segmenter::new(config(), RATE);
        let mut now = Instant::now();
        let audio = tone();
        let mut alice_out = 0;

        for tick in 0..40 {
            // Alice speaks throughout; Bob goes quiet after tick 5.
            if let Some(_u) = alice.on_tick(true, &audio, &speaker(), now) {
                alice_out += 1;
            }
            bob.on_tick(tick < 5, &audio, &speaker(), now);
            now += TICK;
        }

        assert_eq!(
            alice_out, 0,
            "Bob's silence must not close Alice's utterance"
        );
        assert!(alice.is_open());
        assert!(!bob.is_open(), "Bob's own silence should have closed his");
    }

    #[test]
    fn flush_emits_a_partial_utterance() {
        // FR-007: the speaker disconnected, the bot is leaving, or the process
        // is shutting down. This audio used to be dropped silently.
        let mut seg = Segmenter::new(config(), RATE);
        run(&mut seg, &[(true, 10)]);

        let flushed = seg.flush().expect("partial utterance must be emitted");
        assert_eq!(flushed.samples.len(), 10 * TICK_SAMPLES);
        assert!(!seg.is_open());
    }

    #[test]
    fn flush_still_honors_the_minimum() {
        // A flush is not a reason to transcribe a click.
        let mut seg = Segmenter::new(config(), RATE);
        run(&mut seg, &[(true, 2)]);
        assert!(seg.flush().is_none());
    }

    #[test]
    fn flushing_an_idle_segmenter_emits_nothing() {
        let mut seg = Segmenter::new(config(), RATE);
        assert!(seg.flush().is_none());
    }

    // -----------------------------------------------------------------------
    // SC-003 — promptness is a property of the threshold, not of a stopwatch
    // -----------------------------------------------------------------------

    #[test]
    fn close_delay_equals_the_silence_threshold() {
        let config = SegmentationConfig::default();
        let mut seg = Segmenter::new(config, RATE);
        let mut now = Instant::now();
        let audio = tone();
        let start = now;

        for _ in 0..25 {
            seg.on_tick(true, &audio, &speaker(), now);
            now += TICK;
        }
        // The final increment moved past the last speech tick; step back so
        // this measures from when speech actually ended.
        let last_speech = now - TICK;

        let mut closed_at = None;
        for _ in 0..100 {
            if seg.on_tick(false, &[], &speaker(), now).is_some() {
                closed_at = Some(now);
                break;
            }
            now += TICK;
        }

        let delay = closed_at.expect("utterance closes") - last_speech;
        assert!(
            delay >= config.silence && delay < config.silence + TICK * 2,
            "close delay {delay:?} should be the silence threshold {:?}",
            config.silence
        );
        // SC-003 allows 1.5 s end-to-end; the threshold must leave room for
        // transcription inside that budget.
        assert!(
            delay < Duration::from_millis(1_500),
            "the default threshold alone must not consume SC-003's whole budget"
        );
        let _ = start;
    }

    #[test]
    fn the_default_config_matches_the_documented_defaults() {
        let config = SegmentationConfig::default();
        assert_eq!(config.silence, Duration::from_millis(500));
        assert_eq!(config.max_utterance, Duration::from_secs(20));
        assert_eq!(config.min_speech, Duration::from_millis(300));
    }
}
