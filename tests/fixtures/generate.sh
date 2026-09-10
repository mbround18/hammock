#!/usr/bin/env bash
# Regenerate the synthesized (non-speech) audio fixtures.
#
# Task T004. These fixtures exist to catch hallucination: a recognizer handed
# silence, room noise, or music must produce nothing, not invent words. They are
# generated rather than recorded so they are reproducible, license-clean, and
# identical on every machine — re-running this script must produce byte-identical
# output, which is what makes them usable as a regression baseline.
#
# The generated files ARE committed. This script is the record of how, not a
# build step: nothing in CI runs it.
#
# Speech fixtures are NOT generated here. See audio/README.md.
#
# Requires: ffmpeg.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="${here}/audio"
mkdir -p "$out"

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "ffmpeg is required to regenerate fixtures" >&2
  exit 1
fi

# Every fixture is 16 kHz mono signed 16-bit PCM — the format the transcriber
# consumes. The harness Opus round-trips them before transcribing; see
# tests/support/mod.rs.
common=(-ar 16000 -ac 1 -c:a pcm_s16le -y)

echo "==> silence-5s.wav (pure digital silence)"
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i "anullsrc=r=16000:cl=mono" -t 5 \
  "${common[@]}" "$out/silence-5s.wav"

echo "==> noise-fan-10s.wav (steady low-frequency broadband hum)"
# Seeded so it is reproducible. Low-passed to sit where HVAC/fan noise sits
# rather than being flat hiss, and kept quiet enough to be plausible room tone.
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i "anoisesrc=r=16000:c=pink:a=0.08:seed=20260909:d=10" \
  -af "lowpass=f=900,volume=0.6" \
  "${common[@]}" "$out/noise-fan-10s.wav"

echo "==> noise-keyboard-10s.wav (impulsive transients over quiet room tone)"
# Keystroke-like clicks: a seeded noise bed gated into short bursts, which is
# what makes it impulsive rather than continuous. Impulsive noise is the class
# that most often provokes a spurious short transcript.
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i "anoisesrc=r=16000:c=white:a=0.5:seed=20260910:d=10" \
  -af "highpass=f=1800,tremolo=f=7:d=0.98,volume=0.5" \
  "${common[@]}" "$out/noise-keyboard-10s.wav"

echo "==> music-10s.wav (instrumental tone bed)"
# A slow triad progression from pure sines. Not music anyone would enjoy, but
# it is harmonic, sustained, and pitched — the properties that make music a
# hallucination trigger — and it carries no copyright.
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i "sine=frequency=220:sample_rate=16000:duration=10" \
  -f lavfi -i "sine=frequency=277:sample_rate=16000:duration=10" \
  -f lavfi -i "sine=frequency=330:sample_rate=16000:duration=10" \
  -filter_complex "[0][1][2]amix=inputs=3:normalize=0,tremolo=f=0.5:d=0.6,volume=0.25" \
  "${common[@]}" "$out/music-10s.wav"

echo
echo "Wrote:"
ls -l "$out"/*.wav
echo
echo "None of these contain speech. They have no reference transcript by design:"
echo "the expected transcript is empty, and the harness asserts exactly that."
