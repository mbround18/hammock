# Contract: Throughput and Overload on the Telemetry Surface

**Feature**: [../spec.md](../spec.md) | Satisfies **FR-006**, **FR-009**, **FR-010**, **SC-005**, **SC-007**

Extends the `/k8s/metrics` contract established by feature 001
(`specs/001-gpu-accelerated-builds/contracts/telemetry.md`). Additive only: no
existing key changes type or meaning.

## Endpoint

`GET /k8s/metrics` — the existing endpoint, extended. No new route.

The new fields go inside the existing nested `metrics` object, alongside
`total_transcribed_lines` and the rolling windows. They are transcription
statistics, and that is where transcription statistics already live.
`compute_backend` stays a top-level sibling, as feature 001 defined it.

## Added fields

```json
{
  "connected_servers": 2,
  "connected_channels": 1,
  "active_participants": 4,
  "compute_backend": { "kind": "gpu", "device_index": 0, "device_name": "NVIDIA GeForce RTX 4070" },
  "metrics": {
    "uptime_seconds": 3600,
    "total_transcribed_lines": 812,
    "total_transcription_errors": 0,
    "total_utterances_discarded": 17,
    "transcription_queue_depth": 3,
    "transcription_in_flight": 2,
    "transcription_concurrency_limit": 2,
    "transcription_duration_ms": {
      "count": 812,
      "total_ms": 41230,
      "p50_ms": 38,
      "p95_ms": 121,
      "max_ms": 402
    }
  }
}
```

| Key | Type | Meaning |
|---|---|---|
| `total_utterances_discarded` | integer, monotonic | Utterances dropped because the queue was full (FR-006). Non-zero means work was lost. |
| `transcription_queue_depth` | integer, gauge | Jobs enqueued and not yet picked up (FR-009). |
| `transcription_in_flight` | integer, gauge | Jobs currently decoding (FR-009). Never exceeds `transcription_concurrency_limit`. |
| `transcription_concurrency_limit` | integer, constant | The resolved worker count (FR-012). Constant for the process lifetime. |
| `transcription_duration_ms` | object | Distribution of decode wall time (FR-010). |

`total_transcription_errors` already exists, added by feature 001.

### `transcription_duration_ms`

| Key | Type | Meaning |
|---|---|---|
| `count` | integer | Completed transcriptions measured. |
| `total_ms` | integer | Sum, so a mean is derivable without exposing one. |
| `p50_ms`, `p95_ms` | integer | Percentiles over a bounded recent window, not all time. |
| `max_ms` | integer | Largest in the same window. |

Percentiles are over a **recent bounded window**, not process lifetime — a p95
polluted by a slow first decode an hour ago answers nothing about now. This
mirrors the existing `line_windows` approach rather than inventing a second
statistical style in the same payload.

`count` and `total_ms` are lifetime totals; the percentiles are windowed. The two
are deliberately different and the field names say so.

## Answering SC-007: "is the system keeping up?"

The criterion is that an operator can answer this **from metrics alone**. The
fields above are chosen so that they can, in one read:

| Reading | Interpretation |
|---|---|
| `queue_depth` ~0, `in_flight` < limit | Idle capacity. Keeping up comfortably. |
| `queue_depth` ~0, `in_flight` == limit | Saturated but draining. Keeping up. |
| `queue_depth` rising across polls, `discarded` flat | Falling behind, not yet losing work. The warning state. |
| `discarded` rising | Not keeping up. Work is being lost right now. |
| `in_flight` == 0 with `queue_depth` > 0 | The pool is wedged. A defect, not overload. |

The last row is the one that justifies exposing both gauges rather than just
depth: it is the only combination that distinguishes a stuck pool from a busy
one, and they need very different responses.

## Discard logging (FR-007)

Every increment of `total_utterances_discarded` is accompanied by a warning-level
log carrying `guild`, `channel`, and `speaker`. The counter says how much was
lost; the log says whose. SC-005 requires 100% of discards in **both**, so
neither is optional and they increment at the same place — a discard that
incremented the counter without logging would satisfy the metric and fail the
criterion.

## Compatibility

Additive. Consumers reading `compute_backend`, `total_transcribed_lines` or
`line_windows` are unaffected.

## Documentation obligation

`/docs` serves a Swagger document that feature 001 extended with a
`MetricsResponse` schema. The `metrics` object there is currently described
loosely; this feature must give it real properties for the fields above. An
endpoint whose published schema omits fields it returns is a contract violation —
the same obligation feature 001 accepted, applied to its own additions.

## Verification

[../quickstart.md](../quickstart.md) Scenarios 2 and 3. Scenario 3 induces
overload deliberately and asserts the counter, the log line, and the recovery
described by FR-005 and SC-004.
