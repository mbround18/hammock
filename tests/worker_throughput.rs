//! Throughput, state-reuse safety, and concurrency scaling.
//!
//! **Layout note.** `hammock` is a binary crate with no library target, so an
//! integration test cannot reach `TranscriptionHandle` or the worker pool. The
//! queue, shedding and pool mechanics are therefore unit-tested inside `src/`
//! (`transcription::tests`, `transcription::pool::tests`, `config::tests`),
//! where they can touch the real types. What lives here is what genuinely needs
//! a Whisper model and cannot run in-process without one: the FR-003 leak
//! check, and the concurrency scaling measurement behind SC-002.
//!
//! Skips, loudly, when no model is present — `cargo test` is a mandatory CI
//! gate and CI runners have no model, so a hard failure would make the gate
//! unpassable rather than meaningful.

mod support;

use std::{
    sync::{Arc, Barrier},
    time::Instant,
};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn context() -> Option<Arc<WhisperContext>> {
    let model = support::locate_model()?;
    let mut params = WhisperContextParameters::default();
    let use_gpu = std::env::var("WHISPER_USE_GPU")
        .map(|v| v == "true")
        .unwrap_or(false)
        && cfg!(feature = "cuda");
    params.use_gpu(use_gpu);
    WhisperContext::new_with_params(model.to_str()?, params)
        .ok()
        .map(Arc::new)
}

fn decode(state: &mut whisper_rs::WhisperState, audio: &[f32]) -> String {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_progress(false);
    state.full(params, audio).expect("decode");

    let mut text = String::new();
    for idx in 0..state.full_n_segments() {
        if let Some(segment) = state.get_segment(idx) {
            text.push_str(segment.to_str().unwrap_or_default().trim());
            text.push(' ');
        }
    }
    support::normalize_transcript(&text)
}

fn fixture_audio(name: &str) -> Vec<f32> {
    let pcm = support::read_wav_16k_mono(&support::fixture_dir().join(name)).expect("fixture");
    let round_tripped = support::opus_round_trip(&pcm).expect("opus round trip");
    round_tripped
        .iter()
        .map(|s| f32::from(*s) / f32::from(i16::MAX))
        .collect()
}

/// FR-003: reusing decoder state must not leak one utterance into the next.
///
/// This test is the entire enforcement of FR-003. The property holds today only
/// because `no_context` defaults to `true`, which makes whisper.cpp clear
/// `prompt_past` at the top of every decode. A single `set_no_context(false)`
/// anywhere in the decode path would start seeding each transcript with the
/// previous speaker's words — silently, intermittently, and only under the
/// concurrency this feature introduces. Nothing else would catch it.
#[test]
fn reused_state_does_not_leak_between_utterances() {
    let Some(ctx) = context() else {
        println!("{}", support::skip_message());
        return;
    };

    let a = fixture_audio("music-10s.wav");
    let b = fixture_audio("noise-keyboard-10s.wav");

    // B decoded on a state that has never seen anything else.
    let mut fresh = ctx.create_state().expect("state");
    let expected = decode(&mut fresh, &b);

    // B decoded on a state that just handled A.
    let mut reused = ctx.create_state().expect("state");
    let _ = decode(&mut reused, &a);
    let after_reuse = decode(&mut reused, &b);

    assert_eq!(
        after_reuse, expected,
        "decoder state leaked context between utterances: the same audio produced {after_reuse:?} \
         on a reused state but {expected:?} on a fresh one. Check that nothing calls \
         set_no_context(false) — that would put one speaker's words in another's caption."
    );

    // And the reverse direction, so the test cannot pass by both being empty
    // for an unrelated reason.
    let mut reused_back = ctx.create_state().expect("state");
    let a_fresh = {
        let mut fresh_a = ctx.create_state().expect("state");
        decode(&mut fresh_a, &a)
    };
    let _ = decode(&mut reused_back, &b);
    assert_eq!(decode(&mut reused_back, &a), a_fresh);
}

/// SC-002: concurrent decoding raises sustained throughput.
///
/// Measured as a **ratio** between one worker and N, never as an absolute
/// number: absolute throughput is machine-dependent, and asserting it would
/// make this fail on a slow runner for no reason.
#[test]
fn concurrent_workers_raise_throughput() {
    let Some(ctx) = context() else {
        println!("{}", support::skip_message());
        return;
    };

    let audio = Arc::new(fixture_audio("noise-fan-10s.wav"));
    let jobs = 8;

    let serial = run_batch(&ctx, &audio, jobs, 1);
    let concurrent = run_batch(&ctx, &audio, jobs, 4);
    let speedup = serial / concurrent;

    println!(
        "SC-002: {jobs} utterances took {serial:.2}s with 1 worker, {concurrent:.2}s with 4 \
         — {speedup:.2}x"
    );

    assert!(
        speedup > 1.0,
        "4 workers ({concurrent:.2}s) were not faster than 1 ({serial:.2}s); concurrency is \
         not doing anything"
    );
}

/// Decode `jobs` utterances across `workers` threads, each owning one reused
/// state, and return wall-clock seconds. This mirrors the pool's shape: one
/// shared context, one state per worker, reused across jobs.
fn run_batch(ctx: &Arc<WhisperContext>, audio: &Arc<Vec<f32>>, jobs: usize, workers: usize) -> f64 {
    let per_worker = jobs / workers;
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut handles = Vec::new();

    for _ in 0..workers {
        let ctx = Arc::clone(ctx);
        let audio = Arc::clone(audio);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Allocated before the timing window: the pool creates states at
            // startup, so their cost is not part of steady-state throughput.
            let mut state = ctx.create_state().expect("state");
            barrier.wait();
            for _ in 0..per_worker {
                let _ = decode(&mut state, &audio);
            }
        }));
    }

    barrier.wait();
    let started = Instant::now();
    for handle in handles {
        handle.join().expect("worker");
    }
    started.elapsed().as_secs_f64()
}
