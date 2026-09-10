# Contract: Segmentation on the Telemetry Surface

**Feature**: [../spec.md](../spec.md) | Satisfies **FR-011**, **FR-012**

Extends the `/k8s/metrics` contracts from features 001 and 002. Additive only.

## Endpoint

`GET /k8s/metrics` — existing endpoint, extended. No new route.

Both fields go inside the nested `metrics` object, where transcription
statistics already live.

## Added fields

```json
{
  "metrics": {
    "total_utterances_discarded": 0,
    "total_segments_rejected_as_non_speech": 42,
    "utterance_length_ms": {
      "count": 318,
      "total_ms": 1908000,
      "p50_ms": 5200,
      "p95_ms": 17400,
      "max_ms": 20000
    }
  }
}
```

| Key | Type | Meaning |
|---|---|---|
| `total_segments_rejected_as_non_speech` | integer, monotonic | Segments transcribed but producing no caption, because the transcript was empty or annotation-only (FR-012). |
| `utterance_length_ms` | object | Distribution of produced utterance durations (FR-011). |

`utterance_length_ms` uses the same shape as feature 002's
`transcription_duration_ms`: lifetime `count` and `total_ms`, percentiles over a
bounded recent window. The two are different measurements and must not be
confused — one is how long the audio was, the other how long it took to
transcribe.

### Do not confuse with `total_utterances_discarded`

Feature 002 already publishes `total_utterances_discarded`. The two counters mean
different things and an operator must be able to tell them apart:

| Counter | Meaning | What to do |
|---|---|---|
| `total_utterances_discarded` | Dropped because the transcription queue was full. **Work was lost.** | Raise concurrency, or use a smaller model |
| `total_segments_rejected_as_non_speech` | Transcribed, found to contain no speech, no caption written. **Nothing was lost.** | Usually nothing. Sustained growth means an open microphone in a noisy room |

Merging them would be actively harmful: one means the bot is failing and the
other means it is working correctly.

## Answering "is segmentation configured correctly?"

The two fields exist so that both misconfiguration directions are visible without
reading logs or listening to audio:

| Reading | Interpretation |
|---|---|
| `utterance_length_ms.p50` ≈ 5-10 s | Healthy conversational segmentation |
| `p50` collapsing toward `min_speech` | Silence threshold **too low** — utterances fragmenting mid-sentence |
| `p50` pinned at `max_utterance`, `p95` == max | Silence threshold **too high**, or a participant transmitting continuously; boundaries are coming from the length cap rather than from speech |
| `total_segments_rejected_as_non_speech` climbing steadily | An open microphone in a noisy room. Working as intended, but the noise is costing transcription |
| `count` far below expectations for the traffic | Segments are being dropped below `min_speech` — too high a minimum |

The third row is the reason FR-011 asks for a distribution rather than a mean: a
mean of 12 s cannot distinguish healthy long turns from every utterance hitting
the cap, and those need opposite responses.

## SC-005 is read from here

SC-005 asks for a median utterance length of at least 5 s and at least a 50%
reduction in transcription requests per minute of audio. Both come from this
field: `p50_ms >= 5000`, and requests per minute is `count` over uptime compared
against the same figure before the change.

**Caveat**: SC-005 says "for conversational speech", and the fixture corpus has
none (research R0). This field will report a number; that number is only evidence
for SC-005 when the audio behind it is real speech.

## Compatibility

Additive. No existing key changes type or meaning.

## Documentation obligation

The `Metrics` schema in the Swagger document (`src/telemetry/server.rs`, given
real properties by feature 002) must gain both fields in the same change,
including the sentence distinguishing the two counters. An operator reading
`/docs` should not have to guess which one means work was lost.

## Verification

[../quickstart.md](../quickstart.md) Scenarios 2 and 4.
