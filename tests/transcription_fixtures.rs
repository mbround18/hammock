//! Fixture-based transcript accuracy harness.
//!
//! Walks `tests/fixtures/audio/`, puts every fixture through the Opus round trip
//! the deployed bot's audio actually takes, transcribes it, and emits a JSON
//! report of the transcript and word error rate per fixture.
//!
//! This is the mechanism Constitution Principle II requires ("Transcript
//! Accuracy Is Measured, Not Asserted"). Feature 001 uses it for SC-003 —
//! transcripts must agree between the CPU and accelerated variants; features
//! 002-004 each need it for their own criteria.
//!
//! Run it against a specific backend by setting the same environment variables
//! the bot reads:
//!
//! ```sh
//! WHISPER_USE_GPU=false cargo test --test transcription_fixtures -- --nocapture
//! WHISPER_USE_GPU=true  cargo test --features cuda --test transcription_fixtures -- --nocapture
//! ```
//!
//! Skips, loudly, when no model is present. See `support::skip_message`.

mod support;

use std::{fs, path::PathBuf};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Per `tests/fixtures/audio/README.md`, the CPU/GPU tolerance for SC-003.
/// Not used to gate this test — a single run cannot compare two backends — but
/// written into the report so the comparison script has one number to apply.
const CPU_GPU_WER_TOLERANCE: f64 = 0.02;

#[test]
fn fixture_corpus_transcribes_within_tolerance() -> anyhow::Result<()> {
    let Some(model_path) = support::locate_model() else {
        println!("{}", support::skip_message());
        return Ok(());
    };

    let fixtures = support::load_fixtures()?;
    assert!(
        !fixtures.is_empty(),
        "no fixtures found in {} — the corpus is a committed deliverable, not optional",
        support::fixture_dir().display()
    );

    let use_gpu = env_flag("WHISPER_USE_GPU").unwrap_or(cfg!(feature = "cuda"));
    let gpu_device = std::env::var("WHISPER_GPU_DEVICE")
        .ok()
        .and_then(|raw| raw.parse::<i32>().ok())
        .unwrap_or(0);
    let gpu_compiled = cfg!(feature = "cuda");
    let effective_use_gpu = use_gpu && gpu_compiled;

    let mut params = WhisperContextParameters::default();
    params.use_gpu(effective_use_gpu);
    if effective_use_gpu {
        params.gpu_device(gpu_device);
    }

    let model_str = model_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("model path is not valid UTF-8"))?;
    let ctx = WhisperContext::new_with_params(model_str, params)?;

    println!(
        "fixture harness: model={} backend={} ({} fixtures)",
        model_path.display(),
        if effective_use_gpu { "gpu" } else { "cpu" },
        fixtures.len()
    );

    let mut results = Vec::new();
    let mut failures = Vec::new();

    for fixture in &fixtures {
        let pcm = support::read_wav_16k_mono(&fixture.wav_path)?;
        let round_tripped = support::opus_round_trip(&pcm)?;

        let started = std::time::Instant::now();
        let transcript = transcribe(&ctx, &round_tripped)?;
        let elapsed = started.elapsed();

        let hypothesis = support::normalize_transcript(&transcript);
        let audio_seconds = pcm.len() as f64 / f64::from(support::FIXTURE_SAMPLE_RATE);

        let (reference, wer) = match &fixture.reference {
            Some(reference) => {
                let wer = support::word_error_rate(reference, &hypothesis);
                (Some(reference.clone()), Some(wer))
            }
            None => {
                // A non-speech fixture. The recognizer must not invent words.
                // A sound-event annotation — `(soft music)`, `[APPLAUSE]` — is
                // the model correctly reporting that nothing was said, so it
                // passes; bare words do not.
                if !hypothesis.is_empty() && !support::is_sound_event_annotation(&hypothesis) {
                    failures.push(format!(
                        "{}: non-speech fixture produced words, not silence or a sound-event \
                         annotation: {hypothesis:?}",
                        fixture.name
                    ));
                }
                (None, None)
            }
        };

        println!(
            "  {:<26} {:>6.2}s audio  {:>7.2}s wall  {:>6}  {:?}",
            fixture.name,
            audio_seconds,
            elapsed.as_secs_f64(),
            wer.map(|w| format!("{:.1}%", w * 100.0))
                .unwrap_or_else(|| "-".to_string()),
            hypothesis
        );

        results.push(serde_json::json!({
            "fixture": fixture.name,
            "audio_seconds": audio_seconds,
            "wall_seconds": elapsed.as_secs_f64(),
            "expects_empty": fixture.expects_empty(),
            "reference": reference,
            "hypothesis": hypothesis,
            "sound_event_annotation": support::is_sound_event_annotation(&hypothesis),
            "word_error_rate": wer,
        }));
    }

    let report = serde_json::json!({
        "backend": if effective_use_gpu { "gpu" } else { "cpu" },
        "gpu_requested": use_gpu,
        "gpu_compiled": gpu_compiled,
        "gpu_device": effective_use_gpu.then_some(gpu_device),
        "model": model_path.display().to_string(),
        "cpu_gpu_wer_tolerance": CPU_GPU_WER_TOLERANCE,
        "total_wall_seconds": results
            .iter()
            .filter_map(|r| r["wall_seconds"].as_f64())
            .sum::<f64>(),
        "fixtures": results,
    });

    let report_path = report_path(effective_use_gpu);
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_string_pretty(&report)?)?;
    println!("report written to {}", report_path.display());

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// Verifies the round trip is a no-op on duration and does not destroy the
/// signal, without needing a model. Runs everywhere, including CI.
#[test]
fn opus_round_trip_preserves_duration_and_signal() -> anyhow::Result<()> {
    let fixtures = support::load_fixtures()?;
    let noisy = fixtures
        .iter()
        .find(|f| f.name.starts_with("noise-keyboard"))
        .ok_or_else(|| anyhow::anyhow!("the keyboard-noise fixture is missing"))?;

    let pcm = support::read_wav_16k_mono(&noisy.wav_path)?;
    let round_tripped = support::opus_round_trip(&pcm)?;

    assert_eq!(
        pcm.len(),
        round_tripped.len(),
        "the round trip must not change the fixture's duration"
    );

    let energy: f64 = round_tripped
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / round_tripped.len() as f64;
    assert!(
        energy.sqrt() > 100.0,
        "round-tripped audio is near-silent (rms {:.1}) — the codec path is broken, \
         and a broken round trip would make every accuracy measurement meaningless",
        energy.sqrt()
    );

    Ok(())
}

/// Silence must survive the round trip as silence. If the resampler or codec
/// injected energy here, every non-speech fixture assertion would be measuring
/// the harness rather than the recognizer.
#[test]
fn opus_round_trip_keeps_silence_silent() -> anyhow::Result<()> {
    let path = support::fixture_dir().join("silence-5s.wav");
    let pcm = support::read_wav_16k_mono(&path)?;
    let round_tripped = support::opus_round_trip(&pcm)?;

    let peak = round_tripped
        .iter()
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        peak <= 4,
        "silence gained energy in the round trip (peak {peak})"
    );
    Ok(())
}

#[test]
fn sound_event_annotations_are_told_apart_from_invented_words() {
    assert!(support::is_sound_event_annotation("(soft music)"));
    assert!(support::is_sound_event_annotation("[blank_audio]"));
    assert!(support::is_sound_event_annotation("[applause] (laughter)"));
    // Words outside any annotation are what a hallucination looks like.
    assert!(!support::is_sound_event_annotation("thanks for watching"));
    assert!(!support::is_sound_event_annotation(
        "(music) thanks for watching"
    ));
    assert!(!support::is_sound_event_annotation(""));
}

#[test]
fn word_error_rate_scores_the_standard_cases() {
    assert_eq!(support::word_error_rate("", ""), 0.0);
    assert_eq!(support::word_error_rate("", "hello"), 1.0);
    assert_eq!(support::word_error_rate("a b c", "a b c"), 0.0);
    // One substitution out of three reference words.
    assert!((support::word_error_rate("a b c", "a x c") - 1.0 / 3.0).abs() < 1e-9);
    // One deletion.
    assert!((support::word_error_rate("a b c", "a c") - 1.0 / 3.0).abs() < 1e-9);
    // One insertion.
    assert!((support::word_error_rate("a b c", "a b x c") - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn normalization_follows_the_documented_conventions() {
    assert_eq!(
        support::normalize_transcript("  Hello, World!  It's   fine.\n"),
        "hello world it's fine"
    );
    assert_eq!(support::normalize_transcript("[unk] word"), "[unk] word");
    assert_eq!(
        support::normalize_transcript("[BLANK_AUDIO]"),
        "[blank_audio]"
    );
}

fn transcribe(ctx: &WhisperContext, audio: &[i16]) -> anyhow::Result<String> {
    let samples: Vec<f32> = audio
        .iter()
        .map(|s| f32::from(*s) / f32::from(i16::MAX))
        .collect();

    // Mirrors src/transcription.rs exactly. A harness that decodes differently
    // from the bot measures something the bot does not do.
    let language = std::env::var("WHISPER_LANGUAGE").ok();
    let mut state = ctx.create_state()?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language.as_deref());
    params.set_translate(false);
    state.full(params, &samples)?;

    let mut text = String::new();
    for idx in 0..state.full_n_segments() {
        if let Some(segment) = state.get_segment(idx) {
            let piece = segment.to_str()?.trim();
            if piece.is_empty() {
                continue;
            }
            text.push_str(piece);
            text.push(' ');
        }
    }

    let trimmed = text.trim();
    // The bot treats this marker as "nothing was said" and writes no caption;
    // the harness must agree, or every non-speech fixture would fail on a
    // transcript the bot would have discarded.
    if trimmed.eq_ignore_ascii_case("[blank_audio]") {
        return Ok(String::new());
    }
    Ok(trimmed.to_string())
}

fn report_path(gpu: bool) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(if gpu {
            "fixture-report-gpu.json"
        } else {
            "fixture-report-cpu.json"
        })
}

fn env_flag(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .and_then(|raw| match raw.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
}
