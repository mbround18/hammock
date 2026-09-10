//! Shared helpers for the fixture-based accuracy harness.
//!
//! Constitution Principle II requires transcript accuracy to be measured against
//! a committed fixture set rather than asserted. This module is the measuring
//! apparatus: it reads fixtures, puts them through the codec path the deployed
//! bot actually runs, and scores the result against a reference.

#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The rate the transcriber consumes, and the rate every fixture is stored at.
pub const FIXTURE_SAMPLE_RATE: u32 = 16_000;

/// The rate Discord carries Opus at, and the rate the round trip encodes at.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

/// One fixture on disk, paired with its reference transcript if it has one.
pub struct Fixture {
    pub name: String,
    pub wav_path: PathBuf,
    /// `None` for non-speech fixtures, whose expected transcript is empty.
    pub reference: Option<String>,
}

impl Fixture {
    /// A fixture with no reference transcript is a non-speech fixture: the
    /// recognizer is expected to produce nothing at all for it.
    pub fn expects_empty(&self) -> bool {
        self.reference.is_none()
    }
}

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audio")
}

/// Every `.wav` under `tests/fixtures/audio`, sorted so reports are stable
/// across machines and runs.
pub fn load_fixtures() -> anyhow::Result<Vec<Fixture>> {
    let dir = fixture_dir();
    let mut found = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        let reference_path = path.with_extension("txt");
        let reference = if reference_path.exists() {
            Some(normalize_transcript(&fs::read_to_string(&reference_path)?))
        } else {
            None
        };

        found.push(Fixture {
            name,
            wav_path: path,
            reference,
        });
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

// ---------------------------------------------------------------------------
// WAV reading
// ---------------------------------------------------------------------------

/// Read a 16 kHz mono 16-bit PCM WAV file.
///
/// Deliberately strict: a fixture in any other format is rejected rather than
/// converted. Silently resampling a fixture would mean the harness measures a
/// pipeline nobody deployed, which is the exact failure the fixture set exists
/// to prevent.
pub fn read_wav_16k_mono(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = fs::read(path)?;
    anyhow::ensure!(
        bytes.len() >= 44,
        "{}: too short to be a WAV",
        path.display()
    );
    anyhow::ensure!(
        &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "{}: not a RIFF/WAVE file",
        path.display()
    );

    let mut cursor = 12usize;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;

    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into()?) as usize;
        let body_start = cursor + 8;
        let body_end = body_start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .unwrap_or(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => {
                anyhow::ensure!(body.len() >= 16, "{}: truncated fmt chunk", path.display());
                format = Some((
                    u16::from_le_bytes(body[0..2].try_into()?),
                    u16::from_le_bytes(body[2..4].try_into()?),
                    u32::from_le_bytes(body[4..8].try_into()?),
                    u16::from_le_bytes(body[14..16].try_into()?),
                ));
            }
            b"data" => data = Some(body),
            _ => {}
        }

        // Chunks are word-aligned: an odd-sized chunk carries a pad byte.
        cursor = body_start + size + (size & 1);
    }

    let (audio_format, channels, sample_rate, bits) =
        format.ok_or_else(|| anyhow::anyhow!("{}: no fmt chunk", path.display()))?;
    let data = data.ok_or_else(|| anyhow::anyhow!("{}: no data chunk", path.display()))?;

    anyhow::ensure!(
        audio_format == 1,
        "{}: expected uncompressed PCM (format 1), got format {audio_format}",
        path.display()
    );
    anyhow::ensure!(
        channels == 1,
        "{}: expected mono, got {channels} channels — see tests/fixtures/audio/README.md",
        path.display()
    );
    anyhow::ensure!(
        sample_rate == FIXTURE_SAMPLE_RATE,
        "{}: expected {FIXTURE_SAMPLE_RATE} Hz, got {sample_rate} Hz — see tests/fixtures/audio/README.md",
        path.display()
    );
    anyhow::ensure!(
        bits == 16,
        "{}: expected 16-bit samples, got {bits}-bit",
        path.display()
    );

    Ok(data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect())
}

// ---------------------------------------------------------------------------
// Opus round trip
// ---------------------------------------------------------------------------

/// Put 16 kHz mono PCM through the codec path Discord imposes: upsample to
/// 48 kHz, encode to Opus, decode straight back down to 16 kHz mono.
///
/// This is the whole point of the harness. Discord carries Opus at 48 kHz and
/// songbird decodes it before the bot sees a single sample, so a fixture fed to
/// the recognizer as clean PCM measures a pipeline this bot never runs — it
/// would report an accuracy the deployed system does not achieve. Research R5.
///
/// libopus decodes to whatever rate the decoder was constructed with, so the
/// 48 kHz -> 16 kHz leg is done by the codec itself, exactly as it is in
/// production. Only the 16 kHz -> 48 kHz leg is ours, and it is properly
/// band-limited (Constitution: "Resampling introduced anywhere in the pipeline
/// MUST be anti-aliased").
pub fn opus_round_trip(pcm_16k: &[i16]) -> anyhow::Result<Vec<i16>> {
    use opus2::{Application, Channels, Decoder, Encoder};

    let upsampled = upsample_16k_to_48k(pcm_16k);

    let mut encoder = Encoder::new(OPUS_SAMPLE_RATE, Channels::Mono, Application::Voip)
        .map_err(|err| anyhow::anyhow!("opus encoder: {err}"))?;
    let mut decoder = Decoder::new(FIXTURE_SAMPLE_RATE, Channels::Mono)
        .map_err(|err| anyhow::anyhow!("opus decoder: {err}"))?;

    // 20 ms frames — songbird's tick size, and Opus's native voice framing.
    let frame_48k = (OPUS_SAMPLE_RATE as usize / 1000) * 20;
    let frame_16k = (FIXTURE_SAMPLE_RATE as usize / 1000) * 20;

    let mut out = Vec::with_capacity(pcm_16k.len());
    let mut decoded_frame = vec![0i16; frame_16k];

    for chunk in upsampled.chunks(frame_48k) {
        // The final partial frame is zero-padded: Opus only accepts exact frame
        // sizes, and dropping it would silently truncate the fixture.
        let mut frame = chunk.to_vec();
        frame.resize(frame_48k, 0);

        let packet = encoder
            .encode_vec(&frame, 4000)
            .map_err(|err| anyhow::anyhow!("opus encode: {err}"))?;
        let written = decoder
            .decode(&packet, &mut decoded_frame, false)
            .map_err(|err| anyhow::anyhow!("opus decode: {err}"))?;
        out.extend_from_slice(&decoded_frame[..written]);
    }

    // Trim the zero padding introduced by the final partial frame so the round
    // trip does not change the fixture's duration.
    out.truncate(pcm_16k.len());
    Ok(out)
}

/// 3x band-limited interpolation, 16 kHz -> 48 kHz.
///
/// Zero-stuff and low-pass, implemented as a polyphase FIR so only the nonzero
/// taps are evaluated. Naive sample repetition would fold imaging products back
/// into the audible band and hand the encoder a signal the microphone never
/// produced.
fn upsample_16k_to_48k(input: &[i16]) -> Vec<i16> {
    const FACTOR: usize = 3;
    /// Half-length in output samples. 32 gives ~65 dB of image rejection here,
    /// which is far below Opus's own noise floor at voice bitrates.
    const HALF: usize = 32;

    // Cut just below the 8 kHz Nyquist of the 16 kHz source, expressed against
    // the 48 kHz output rate.
    let cutoff = 7_600.0 / OPUS_SAMPLE_RATE as f64;
    let taps = build_lowpass(HALF * FACTOR, cutoff, FACTOR as f64);

    let mut output = Vec::with_capacity(input.len() * FACTOR);
    let center = taps.len() / 2;

    for out_idx in 0..input.len() * FACTOR {
        let mut acc = 0.0f64;
        // Only every FACTOR-th input to the filter is nonzero after
        // zero-stuffing; walk those directly instead of multiplying by zero.
        let phase = out_idx % FACTOR;
        let base = out_idx / FACTOR;
        let mut tap = phase;
        while tap < taps.len() {
            let offset = (tap as isize - center as isize) / FACTOR as isize;
            let src = base as isize - offset;
            if src >= 0 && (src as usize) < input.len() {
                acc += taps[tap] * f64::from(input[src as usize]);
            }
            tap += FACTOR;
        }
        output.push(acc.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16);
    }

    output
}

/// Windowed-sinc low-pass. `gain` compensates for the energy lost to
/// zero-stuffing, which divides amplitude by the interpolation factor.
fn build_lowpass(half_len: usize, cutoff: f64, gain: f64) -> Vec<f64> {
    let len = half_len * 2 + 1;
    let center = half_len as f64;
    let mut taps = Vec::with_capacity(len);

    for idx in 0..len {
        let n = idx as f64 - center;
        let sinc = if n.abs() < f64::EPSILON {
            2.0 * cutoff
        } else {
            (2.0 * std::f64::consts::PI * cutoff * n).sin() / (std::f64::consts::PI * n)
        };
        // Hamming window — flat enough in-band and steep enough out of it for a
        // filter whose output is about to be handed to a lossy codec anyway.
        let window =
            0.54 - 0.46 * (2.0 * std::f64::consts::PI * idx as f64 / (len - 1) as f64).cos();
        taps.push(sinc * window);
    }

    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap = *tap / sum * gain;
    }
    taps
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Reduce a transcript to comparable tokens.
///
/// Implements the conventions documented in `tests/fixtures/audio/README.md`.
/// Both sides of every comparison go through this — a WER computed between
/// differently normalized strings measures punctuation, not recognition.
pub fn normalize_transcript(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut out = String::with_capacity(lowered.len());

    for ch in lowered.chars() {
        // Brackets, parentheses and underscores survive so markers stay intact:
        // `[unk]` in a reference, `[blank_audio]` or `(soft music)` from the
        // recognizer. Stripping them would turn a sound-event annotation into
        // what looks like two ordinary words, which is precisely the
        // distinction `is_sound_event_annotation` needs to make.
        if ch.is_alphanumeric()
            || ch == '\''
            || ch == '['
            || ch == ']'
            || ch == '('
            || ch == ')'
            || ch == '_'
        {
            out.push(ch);
        } else {
            // Everything else — punctuation, newlines, runs of spaces — is a
            // token separator.
            out.push(' ');
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Is this transcript nothing but sound-event annotations?
///
/// **Mirrors `is_sound_event_annotation` in `src/transcription/mod.rs`, which is
/// the source of truth.** The duplication is forced: `hammock` is a binary crate
/// with no library target, so an integration test cannot reach production code.
/// If the rule changes, change both — the production copy decides what reaches
/// caption files, this one only decides what this harness reports.
///
/// Whisper narrates non-speech audio rather than staying silent: handed music
/// it emits `(soft music)`, handed applause `[APPLAUSE]`. That is the model
/// correctly reporting *no speech was said* — categorically different from
/// hallucinating words that were never spoken, which is what the non-speech
/// fixtures exist to catch.
///
/// So the two are told apart rather than lumped together. A non-speech fixture
/// may produce annotations; it may not produce bare words.
///
/// Note what this does *not* claim: the bot itself only filters
/// `[blank_audio]`, so an annotation like `(soft music)` does reach a caption
/// file today. That is a real behavior, recorded in the harness report — but it
/// is a caption-filtering concern, not a GPU one, and fixing it here would
/// change transcript output under a feature that is supposed to leave it alone.
pub fn is_sound_event_annotation(normalized: &str) -> bool {
    if normalized.is_empty() {
        return false;
    }

    let mut depth = 0i32;
    let mut saw_group = false;
    let mut outside_group_has_text = false;

    for ch in normalized.chars() {
        match ch {
            '[' | '(' => {
                depth += 1;
                saw_group = true;
            }
            ']' | ')' => depth = (depth - 1).max(0),
            ch if ch.is_whitespace() => {}
            _ if depth == 0 => outside_group_has_text = true,
            _ => {}
        }
    }

    saw_group && !outside_group_has_text
}

/// Word error rate: (substitutions + deletions + insertions) / reference words.
///
/// Returns 0.0 when both sides are empty (a non-speech fixture the recognizer
/// correctly said nothing about) and 1.0 when the reference is empty but the
/// hypothesis is not — a hallucination is a total failure for that fixture, and
/// dividing by a zero-length reference would otherwise be undefined.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let reference: Vec<&str> = reference.split_whitespace().collect();
    let hypothesis: Vec<&str> = hypothesis.split_whitespace().collect();

    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }

    // Levenshtein over words, two rows rather than a full matrix.
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0usize; hypothesis.len() + 1];

    for (r_idx, r_word) in reference.iter().enumerate() {
        current[0] = r_idx + 1;
        for (h_idx, h_word) in hypothesis.iter().enumerate() {
            let substitution = previous[h_idx] + usize::from(r_word != h_word);
            let deletion = previous[h_idx + 1] + 1;
            let insertion = current[h_idx] + 1;
            current[h_idx + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[hypothesis.len()] as f64 / reference.len() as f64
}

// ---------------------------------------------------------------------------
// Model discovery
// ---------------------------------------------------------------------------

/// Where the Whisper model is, resolved the same way the bot resolves it.
///
/// Returns `None` when no model is present. The harness skips rather than fails
/// in that case: `cargo test` is a mandatory CI gate (constitution, Development
/// Workflow) and CI runners have no multi-hundred-megabyte model on disk, so a
/// hard failure here would make the gate unpassable rather than meaningful.
pub fn locate_model() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("WHISPER_MODEL_PATH")
        && !explicit.is_empty()
    {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }

    let dir = std::env::var("WHISPER_MODEL_DIR").unwrap_or_else(|_| "models".to_string());
    let name = std::env::var("WHISPER_MODEL_NAME").unwrap_or_else(|_| "base".to_string());
    let path = Path::new(&dir).join(format!("ggml-{name}.bin"));
    path.exists().then_some(path)
}

/// The message printed when the harness skips. Says what is missing and how to
/// fix it, so a skipped accuracy gate is never mistaken for a passing one.
pub fn skip_message() -> String {
    format!(
        "SKIPPED: no Whisper model found, so transcript accuracy was NOT measured.\n\
         Looked at $WHISPER_MODEL_PATH, then {}/ggml-{}.bin.\n\
         To run this harness, download a model:\n  \
         curl -L -o models/ggml-base.bin \\\n    \
         https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        std::env::var("WHISPER_MODEL_DIR").unwrap_or_else(|_| "models".to_string()),
        std::env::var("WHISPER_MODEL_NAME").unwrap_or_else(|_| "base".to_string()),
    )
}
