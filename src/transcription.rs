use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serenity::model::id::{ChannelId, GuildId, UserId};
use tokio::sync::mpsc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};

use crate::captions::{CaptionEntry, CaptionSink, SpeakerInfo};
use whisper_rs::WhisperContextParameters;

const PCM_NORMALIZER: f32 = i16::MAX as f32;
const WHISPER_SAMPLE_RATE: u32 = 16_000;

pub struct TranscriptionJob {
    pub channel_id: ChannelId,
    pub guild_id: GuildId,
    pub speaker_id: Option<UserId>,
    pub speaker_name: String,
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
    pub started_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct TranscriptionHandle {
    tx: mpsc::Sender<TranscriptionJob>,
}

impl TranscriptionHandle {
    pub async fn submit(&self, job: TranscriptionJob) -> anyhow::Result<()> {
        self.tx
            .send(job)
            .await
            .context("transcription queue dropped")
    }
}

pub fn spawn_worker(
    model_path: PathBuf,
    sink: Arc<CaptionSink>,
    language: Option<String>,
    use_gpu: bool,
    gpu_device: i32,
) -> anyhow::Result<TranscriptionHandle> {
    let (tx, mut rx) = mpsc::channel::<TranscriptionJob>(32);
    let model_path_str = model_path
        .to_str()
        .context("WHISPER_MODEL_PATH must be valid UTF-8")?
        .to_owned();
    let gpu_compiled = cfg!(feature = "cuda");
    let effective_use_gpu = use_gpu && gpu_compiled;
    if use_gpu && !gpu_compiled {
        tracing::warn!(
            "GPU transcription requested but the cuda feature is not enabled; falling back to CPU"
        );
    }

    let mut ctx_params = WhisperContextParameters::default();
    ctx_params.use_gpu(effective_use_gpu);
    if effective_use_gpu {
        ctx_params.gpu_device(gpu_device);
    }
    let ctx = Arc::new(
        WhisperContext::new_with_params(&model_path_str, ctx_params)
            .context("loading Whisper model")?,
    );

    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let ctx = Arc::clone(&ctx);
            let sink = Arc::clone(&sink);
            let language = language.clone();
            if let Err(err) = tokio::task::spawn_blocking(move || {
                if let Err(inner) = transcribe_and_write(ctx, sink, job, language.as_deref()) {
                    tracing::error!("transcription failed: {inner:?}");
                }
            })
            .await
            {
                tracing::error!("transcription task join error: {err}");
            }
        }
    });

    Ok(TranscriptionHandle { tx })
}

fn transcribe_and_write(
    ctx: Arc<WhisperContext>,
    sink: Arc<CaptionSink>,
    job: TranscriptionJob,
    language: Option<&str>,
) -> anyhow::Result<()> {
    if job.pcm.is_empty() {
        return Ok(());
    }

    let audio = prepare_audio(&job.pcm, job.sample_rate);
    let mut state = ctx.create_state()?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language);
    params.set_translate(false);

    state.full(params, &audio)?;

    let mut text = String::new();
    let segments = state.full_n_segments();
    for idx in 0..segments {
        if let Some(segment) = state.get_segment(idx) {
            let segment_text = segment.to_str()?.trim();
            if segment_text.is_empty() {
                continue;
            }
            text.push_str(segment_text);
            text.push(' ');
        }
    }

    let normalized = text.trim();
    if normalized.is_empty() {
        return Ok(());
    }

    if normalized.eq_ignore_ascii_case("[blank_audio]") {
        return Ok(());
    }

    let timestamp = job.started_at.format("%Y-%m-%dT%H:%M:%S").to_string();
    let entry = CaptionEntry {
        speaker: SpeakerInfo {
            id: job.speaker_id,
            name: job.speaker_name.clone(),
        },
        comment: normalized.to_string(),
        timestamp,
    };
    sink.append_json(job.guild_id, job.channel_id, entry)?;
    Ok(())
}

fn prepare_audio(samples: &[i16], sample_rate: u32) -> Vec<f32> {
    if sample_rate == WHISPER_SAMPLE_RATE {
        return pcm_to_f32(samples);
    }

    let ratio = sample_rate as f32 / WHISPER_SAMPLE_RATE as f32;
    let target_len = ((samples.len() as f32) / ratio).ceil() as usize;
    let mut downsampled = Vec::with_capacity(target_len);
    for idx in 0..target_len {
        let source_idx = ((idx as f32) * ratio).floor() as usize;
        if let Some(sample) = samples.get(source_idx) {
            downsampled.push(*sample);
        }
    }
    pcm_to_f32(&downsampled)
}

fn pcm_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|s| f32::from(*s) / PCM_NORMALIZER)
        .collect()
}
