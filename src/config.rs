use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, anyhow, bail};
use which::which;

const DEFAULT_ENTRY_SOUND_VOLUME: f32 = 0.5;

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub discord_token: String,
    pub whisper_model_path: PathBuf,
    pub caption_dir: PathBuf,
    /// Retained only so an operator who set `CAPTION_CHUNK_SECS` gets told it
    /// no longer does anything. Segmentation follows speech now.
    pub chunk_duration: Duration,
    pub sample_rate: u32,
    pub whisper_language: Option<String>,
    pub whisper_cli_path: Option<PathBuf>,
    pub whisper_model_name: String,
    pub whisper_use_gpu: bool,
    pub whisper_gpu_device: i32,
    /// How many utterances may be transcribed at once. `None` means "use the
    /// default for whichever compute backend turns out to be active", which is
    /// not known until the backend is resolved at worker construction.
    pub transcription_concurrency: Option<usize>,
    /// Quiet after which an utterance is closed (FR-002, FR-003).
    pub utterance_silence: Duration,
    /// Longest an utterance may run before being split (FR-004).
    pub utterance_max: Duration,
    /// Speech shorter than this is not transcribed (FR-005).
    pub utterance_min_speech: Duration,
    pub entry_sound_path: PathBuf,
    pub entry_sound_volume: f32,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    pub include_transcripts_with_summary: bool,
    pub http_bind_addr: SocketAddr,
}

impl BotConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let discord_token =
            env::var("DISCORD_TOKEN").context("Missing DISCORD_TOKEN in environment")?;
        let whisper_cli_path = env::var("WHISPER_CLI_PATH")
            .ok()
            .map(PathBuf::from)
            .map(Self::absolute_path)
            .transpose()?;
        let caption_dir = env::var("CAPTION_OUTPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("captions"));
        // CAPTION_CHUNK_SECS used to do two jobs: it set the fixed chunk size,
        // and via `silence_flush: state.chunk_duration` it set the silence
        // timeout. Speech-aware segmentation took both away.
        //
        // It is kept rather than removed, and it warns rather than staying
        // quiet. Removing it would break `.env` files that set it; accepting it
        // silently would leave an operator who had tuned it believing it still
        // worked. Neither is acceptable, so it says so.
        let chunk_secs = env::var("CAPTION_CHUNK_SECS")
            .ok()
            .and_then(|raw| raw.parse::<f32>().ok())
            .map(|secs| {
                tracing::warn!(
                    "CAPTION_CHUNK_SECS={secs} no longer affects segmentation. Utterance \
                     boundaries now follow speech, not a fixed chunk length. Use \
                     UTTERANCE_SILENCE_MS to control when an utterance closes and \
                     UTTERANCE_MAX_SECS to bound its length."
                );
                secs.max(0.5)
            })
            .unwrap_or(3.0);
        let sample_rate = env::var("DECODE_SAMPLE_RATE")
            .ok()
            .and_then(|raw| raw.parse::<u32>().ok())
            .filter(|rate| *rate > 0)
            .unwrap_or(16_000);
        let whisper_language = env::var("WHISPER_LANGUAGE").ok();
        let whisper_model_name =
            env::var("WHISPER_MODEL_NAME").unwrap_or_else(|_| "base".to_string());
        let whisper_use_gpu = env::var("WHISPER_USE_GPU")
            .ok()
            .and_then(|raw| Self::parse_bool(&raw))
            .unwrap_or(cfg!(feature = "cuda"));
        let whisper_gpu_device = env::var("WHISPER_GPU_DEVICE")
            .ok()
            .and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or(0);

        let whisper_model_path = match env::var("WHISPER_MODEL_PATH") {
            Ok(raw) => Self::absolute_path(PathBuf::from(raw))?,
            Err(_) => {
                let model_dir = env::var("WHISPER_MODEL_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("models"));
                let resolved_dir = Self::absolute_path(model_dir)?;
                let filename = format!("ggml-{}.bin", whisper_model_name);
                resolved_dir.join(filename)
            }
        };

        let entry_sound_path = env::var("ENTRY_SOUND_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("resources/announce.mp3"));
        let entry_sound_path = Self::absolute_path(entry_sound_path)?;
        let entry_sound_volume = env::var("ENTRY_SOUND_VOLUME")
            .ok()
            .and_then(|raw| raw.parse::<f32>().ok())
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(DEFAULT_ENTRY_SOUND_VOLUME);
        let openai_api_key = env::var("OPENAPI_KEY").ok();
        let openai_model = env::var("OPENAPI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        let include_transcripts_with_summary = env::var("INCLUDE_TRANSCRIPTS_WITH_SUMMARY")
            .ok()
            .and_then(|raw| Self::parse_bool(&raw))
            .unwrap_or(true);
        let transcription_concurrency = Self::parse_concurrency()?;
        let (utterance_silence, utterance_max, utterance_min_speech) = Self::parse_segmentation()?;
        let http_bind_addr = env::var("HTTP_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
            .parse()
            .context("Invalid HTTP_BIND_ADDR value")?;

        if openai_api_key.is_none() && !include_transcripts_with_summary {
            bail!(
                "INCLUDE_TRANSCRIPTS_WITH_SUMMARY=false requires OPENAPI_KEY; summary-only flow is not possible without an OpenAI key"
            );
        }

        Ok(Self {
            discord_token,
            whisper_model_path,
            caption_dir,
            chunk_duration: Duration::from_secs_f32(chunk_secs),
            sample_rate,
            whisper_language,
            whisper_cli_path,
            whisper_model_name,
            whisper_use_gpu,
            whisper_gpu_device,
            transcription_concurrency,
            utterance_silence,
            utterance_max,
            utterance_min_speech,
            entry_sound_path,
            entry_sound_volume,
            openai_api_key,
            openai_model,
            include_transcripts_with_summary,
            http_bind_addr,
        })
    }

    /// Parse and validate `TRANSCRIPTION_CONCURRENCY`.
    ///
    /// Refuses to start on a bad value, unlike the GPU settings, which degrade
    /// to CPU. The difference is deliberate: an absent GPU is an environment
    /// problem the operator may not control, so Principle III requires
    /// degrading. A nonsense concurrency value is a configuration problem they
    /// control and can fix in seconds — and silently substituting a value would
    /// leave them believing a setting took effect when it did not, which
    /// FR-011 rules out in as many words.
    ///
    /// Every error names the offending value *and* the accepted range, per
    /// Constitution Principle IV.
    fn parse_concurrency() -> anyhow::Result<Option<usize>> {
        const MAX: usize = crate::transcription::pool::MAX_CONCURRENCY;

        let raw = match env::var("TRANSCRIPTION_CONCURRENCY") {
            Ok(raw) if !raw.trim().is_empty() => raw,
            // Unset or empty: the backend default applies.
            _ => return Ok(None),
        };
        let raw = raw.trim();

        let value: i64 = raw.parse().map_err(|_| {
            anyhow::anyhow!(
                "TRANSCRIPTION_CONCURRENCY='{raw}' is not a positive integer. \
                 Accepted range is 1 to {MAX}."
            )
        })?;

        if value == 0 {
            bail!(
                "TRANSCRIPTION_CONCURRENCY=0 is not valid: transcription would never run. \
                 Accepted range is 1 to {MAX}."
            );
        }
        if value < 0 {
            bail!(
                "TRANSCRIPTION_CONCURRENCY={value} is not a positive integer. \
                 Accepted range is 1 to {MAX}."
            );
        }
        if value as usize > MAX {
            bail!("TRANSCRIPTION_CONCURRENCY={value} exceeds the maximum of {MAX}.");
        }

        Ok(Some(value as usize))
    }

    /// Accepted ranges for the segmentation tunables, published in
    /// `contracts/configuration.md` and in `.env.sample` rather than merely
    /// enforced — the spec's edge case asks for a documented safe range, not
    /// just a bounded one.
    const SILENCE_MS_RANGE: (u64, u64) = (100, 5_000);
    const MAX_SECS_RANGE: (u64, u64) = (1, 30);
    const MIN_SPEECH_MS_RANGE: (u64, u64) = (0, 5_000);

    /// Below this, utterances fragment mid-sentence. Permitted, but warned
    /// about, because it silently undoes the feature it configures.
    const SILENCE_MS_ADVISORY_FLOOR: u64 = 200;

    /// Parse and validate the three segmentation tunables.
    ///
    /// Refuses to start on a bad value, matching `TRANSCRIPTION_CONCURRENCY`
    /// and differing from the GPU settings for the same reason: an absent GPU
    /// is an environment problem the operator may not control, so it degrades.
    /// A nonsense duration is a configuration problem they control and can fix
    /// in seconds, and silently substituting a value would leave them believing
    /// a setting took effect when it did not.
    fn parse_segmentation() -> anyhow::Result<(Duration, Duration, Duration)> {
        let silence_ms =
            Self::parse_bounded_u64("UTTERANCE_SILENCE_MS", 500, Self::SILENCE_MS_RANGE, None)?;
        let max_secs = Self::parse_bounded_u64(
            "UTTERANCE_MAX_SECS",
            20,
            Self::MAX_SECS_RANGE,
            Some("the recognizer processes a 30-second window and would truncate the excess"),
        )?;
        let min_speech_ms = Self::parse_bounded_u64(
            "UTTERANCE_MIN_SPEECH_MS",
            300,
            Self::MIN_SPEECH_MS_RANGE,
            None,
        )?;

        // The cross-field rule, which no per-variable check can catch: two
        // individually valid values can combine into a bot that transcribes
        // nothing at all and reports no error. That is precisely the "failing
        // later at use" Constitution Principle IV forbids.
        if min_speech_ms >= max_secs * 1_000 {
            bail!(
                "UTTERANCE_MIN_SPEECH_MS={min_speech_ms} is not below UTTERANCE_MAX_SECS={max_secs} \
                 ({}ms): no utterance could ever be long enough to transcribe, so the bot would \
                 run and caption nothing.",
                max_secs * 1_000
            );
        }

        if silence_ms < Self::SILENCE_MS_ADVISORY_FLOOR {
            tracing::warn!(
                "UTTERANCE_SILENCE_MS={silence_ms} is below {}ms; utterances will fragment \
                 mid-sentence. Captions will contain more, shorter lines and word error rate \
                 will rise.",
                Self::SILENCE_MS_ADVISORY_FLOOR
            );
        }

        Ok((
            Duration::from_millis(silence_ms),
            Duration::from_secs(max_secs),
            Duration::from_millis(min_speech_ms),
        ))
    }

    /// Read an integer environment variable, or its default, rejecting anything
    /// outside `range` with a message naming both the value and the range.
    ///
    /// `hint` adds a clause explaining *why* a bound is where it is, for the
    /// cases where the number would otherwise look arbitrary.
    fn parse_bounded_u64(
        key: &str,
        default: u64,
        range: (u64, u64),
        hint: Option<&str>,
    ) -> anyhow::Result<u64> {
        let (min, max) = range;

        let raw = match env::var(key) {
            Ok(raw) if !raw.trim().is_empty() => raw,
            _ => return Ok(default),
        };
        let raw = raw.trim();

        let value: i64 = raw.parse().map_err(|_| {
            anyhow!("{key}='{raw}' is not a positive integer. Accepted range is {min} to {max}.")
        })?;

        if value < 0 {
            bail!("{key}={value} is not a positive integer. Accepted range is {min} to {max}.");
        }
        let value = value as u64;

        if value < min {
            bail!("{key}={value} is below the minimum of {min}.");
        }
        if value > max {
            match hint {
                Some(hint) => bail!("{key}={value} exceeds the maximum of {max} ({hint})."),
                None => bail!("{key}={value} exceeds the maximum of {max}."),
            }
        }

        Ok(value)
    }

    #[allow(dead_code)] // Retained with `chunk_duration`; see its comment.
    pub fn chunk_samples(&self) -> usize {
        let samples = self.chunk_duration.as_secs_f64() * f64::from(self.sample_rate);
        samples.max(1.0).round() as usize
    }
}

impl BotConfig {
    fn absolute_path(path: PathBuf) -> anyhow::Result<PathBuf> {
        if path.is_absolute() {
            return Ok(path);
        }

        let cwd = env::current_dir().context("Unable to read current directory")?;
        Ok(cwd.join(path))
    }

    pub fn locate_whisper_cli(&self) -> anyhow::Result<PathBuf> {
        if let Some(path) = &self.whisper_cli_path {
            return Ok(path.clone());
        }

        which("whisper").map_err(|_| {
            anyhow!("Whisper CLI not found. Set WHISPER_CLI_PATH or add `whisper` to PATH")
        })
    }

    pub fn whisper_model_name(&self) -> &str {
        &self.whisper_model_name
    }

    fn parse_bool(raw: &str) -> Option<bool> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_concurrency` reads process-wide environment state, so these cases
    /// share one test rather than racing each other across threads.
    #[test]
    fn concurrency_validation_names_the_value_and_the_range() {
        let max = crate::transcription::pool::MAX_CONCURRENCY;

        // SAFETY: single-threaded within this test, and every case restores the
        // variable before returning.
        unsafe {
            env::remove_var("TRANSCRIPTION_CONCURRENCY");
            assert_eq!(BotConfig::parse_concurrency().unwrap(), None);

            env::set_var("TRANSCRIPTION_CONCURRENCY", "   ");
            assert_eq!(
                BotConfig::parse_concurrency().unwrap(),
                None,
                "an empty value means 'unset', not 'zero'"
            );

            env::set_var("TRANSCRIPTION_CONCURRENCY", "6");
            assert_eq!(BotConfig::parse_concurrency().unwrap(), Some(6));

            env::set_var("TRANSCRIPTION_CONCURRENCY", "0");
            let err = BotConfig::parse_concurrency().unwrap_err().to_string();
            assert!(err.contains("never run"), "{err}");
            assert!(err.contains(&max.to_string()), "must name the range: {err}");

            env::set_var("TRANSCRIPTION_CONCURRENCY", "abc");
            let err = BotConfig::parse_concurrency().unwrap_err().to_string();
            assert!(err.contains("abc"), "must name the bad value: {err}");
            assert!(err.contains(&max.to_string()), "must name the range: {err}");

            env::set_var("TRANSCRIPTION_CONCURRENCY", "-3");
            let err = BotConfig::parse_concurrency().unwrap_err().to_string();
            assert!(err.contains("-3"), "must name the bad value: {err}");

            env::set_var("TRANSCRIPTION_CONCURRENCY", (max + 1).to_string());
            let err = BotConfig::parse_concurrency().unwrap_err().to_string();
            assert!(err.contains("maximum"), "{err}");

            env::remove_var("TRANSCRIPTION_CONCURRENCY");
        }
    }
}
