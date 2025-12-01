use std::{env, path::PathBuf, time::Duration};

use anyhow::{Context, anyhow, bail};
use which::which;

const DEFAULT_ENTRY_SOUND_VOLUME: f32 = 0.5;

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub discord_token: String,
    pub whisper_model_path: PathBuf,
    pub caption_dir: PathBuf,
    pub chunk_duration: Duration,
    pub sample_rate: u32,
    pub whisper_language: Option<String>,
    pub whisper_cli_path: Option<PathBuf>,
    pub whisper_model_name: String,
    pub whisper_use_gpu: bool,
    pub whisper_gpu_device: i32,
    pub entry_sound_path: PathBuf,
    pub entry_sound_volume: f32,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    pub include_transcripts_with_summary: bool,
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
        let chunk_secs = env::var("CAPTION_CHUNK_SECS")
            .ok()
            .and_then(|raw| raw.parse::<f32>().ok())
            .map(|secs| secs.max(0.5))
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
            entry_sound_path,
            entry_sound_volume,
            openai_api_key,
            openai_model,
            include_transcripts_with_summary,
        })
    }

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
