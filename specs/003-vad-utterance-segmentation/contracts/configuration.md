# Contract: Segmentation Tunables

**Feature**: [../spec.md](../spec.md) | Satisfies **FR-010**

Three new variables. Constitution Principle IV governs their shape: a documented
default safe for a CPU-only self-hoster, an entry in `.env.sample` explaining the
effect, and validation at startup with an actionable error rather than a failure
later at use.

The spec's edge case — "what happens when the silence threshold is configured
very low? Utterances fragment; the configuration must have a documented safe
range" — is why ranges are published here rather than merely enforced.

## Variables

| Variable | Default | Range | Effect |
|---|---|---|---|
| `UTTERANCE_SILENCE_MS` | `500` | 100-5000 | Quiet after which an utterance is closed |
| `UTTERANCE_MAX_SECS` | `20` | 1-30 | Longest an utterance may run before being split |
| `UTTERANCE_MIN_SPEECH_MS` | `300` | 0-5000 | Speech shorter than this is not transcribed |

### `UTTERANCE_SILENCE_MS` — 500 ms

The one that matters most, and the one most tempting to lower.

Too low and utterances fragment mid-sentence, which is the defect this feature
exists to fix — a 200 ms threshold splits on an ordinary breath. Too high and
captions lag: this value sits directly inside SC-003's 1.5 s budget, because the
delay between someone finishing and their caption appearing is this threshold
plus transcription time.

500 ms is long enough to survive an intra-sentence pause (FR-003), short enough
to leave a second of SC-003's budget for transcription. Twenty-five voice ticks.

**Below 200 ms is a supported setting but a bad idea**, and the log says so at
startup rather than leaving it to be discovered in the transcript.

### `UTTERANCE_MAX_SECS` — 20 s

whisper.cpp processes a **fixed 30-second window per request regardless of input
length**, so audio beyond 30 s is not merely wasteful, it is truncated. The
maximum must stay below that with headroom, since a split lands on the most
recent quiet tick rather than exactly on the limit.

It is also the memory bound: 20 s of 16 kHz mono is 640 KB per speaking
participant, which is what keeps the tick-path buffer from growing without limit
(Constitution Principle I).

**Capped at 30** because beyond it the recognizer silently discards the excess,
and a setting whose only effect is to lose audio should not be reachable.

### `UTTERANCE_MIN_SPEECH_MS` — 300 ms

Below roughly this, a segment is a click, a cough, or a single syllable, and
handing it to the recognizer invites an invented word — the failure Story 3 is
about.

`0` disables the filter and is permitted: an operator who would rather have
fragments than lose a clipped "yes" can have that.

## Validation

Checked at startup, before the voice pipeline is built. Each failure names the
value received **and** the accepted range:

| Input | Behavior |
|---|---|
| unset or empty | Use the default. |
| in range | Use it. |
| not an integer | Refuse: `UTTERANCE_SILENCE_MS='abc' is not a positive integer. Accepted range is 100 to 5000.` |
| out of range | Refuse: `UTTERANCE_MAX_SECS=45 exceeds the maximum of 30 (the recognizer processes a 30-second window and would truncate the excess).` |
| `UTTERANCE_MIN_SPEECH_MS` ≥ `UTTERANCE_MAX_SECS` | Refuse: nothing would ever be transcribed. |

**Refusing to start matches feature 002's `TRANSCRIPTION_CONCURRENCY` and differs
from feature 001's GPU settings, for the same reason**: an absent GPU is an
environment problem the operator may not control, so it degrades. A nonsense
duration is a configuration problem they control and can fix in seconds, and
silently substituting a value would leave them believing a setting took effect
when it did not.

The cross-field rule is the one a per-variable check would miss. Two individually
valid values — a 5000 ms minimum and a 1 s maximum — combine into a bot that
transcribes nothing at all and reports no error, which is exactly the "failing
later at use" that Principle IV forbids.

## Startup report

Alongside the backend and concurrency lines from features 001 and 002:

```text
INFO transcription backend: CPU
INFO transcription concurrency: 4 workers (default for cpu backend, 32 cores available)
INFO utterance segmentation: close after 500ms silence, max 20s, min speech 300ms
```

An aggressive setting is called out rather than merely accepted:

```text
WARN UTTERANCE_SILENCE_MS=150 is below 200ms; utterances will fragment mid-sentence.
     Captions will contain more, shorter lines and word error rate will rise.
```

## Relationship to `CAPTION_CHUNK_SECS`

`CAPTION_CHUNK_SECS` currently does two jobs: it sets the fixed chunk size and,
via `silence_flush: state.chunk_duration` (`src/main.rs:543`), the silence
timeout. **This feature takes both away** — boundaries come from speech, and the
silence timeout is now `UTTERANCE_SILENCE_MS`.

Whether the variable is removed or retained as a no-op is a task-phase decision.
Retaining it silently would be the worse option: an operator who has tuned it
would see their setting stop working with no explanation. If it is retained, it
should warn that it no longer affects segmentation.

## `.env.sample`

All three documented together, with the effect of raising and lowering each, the
safe ranges, and a pointer to `utterance_length_ms` on `/k8s/metrics` as the way
to tell whether the current values are right.
