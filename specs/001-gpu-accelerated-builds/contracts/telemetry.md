# Contract: Compute Backend on the Telemetry Surface

**Feature**: [../spec.md](../spec.md) | Satisfies **FR-007**

The bot's operational state is exposed over HTTP by `src/telemetry/server.rs`.
FR-007 requires the active compute backend be discoverable there, not only in
startup logs that scroll away.

## Endpoint

`GET /k8s/metrics` — the existing endpoint, extended. No new route is added.

The current response is a `MetricsResponse` object carrying
`connected_servers`, `connected_channels`, `active_participants`, and a nested
`metrics` object. This contract adds one sibling key.

## Added field

```json
{
  "connected_servers": 2,
  "connected_channels": 1,
  "active_participants": 4,
  "metrics": { "...": "unchanged" },
  "compute_backend": {
    "kind": "gpu",
    "device_index": 0,
    "device_name": "NVIDIA GeForce RTX 4070"
  }
}
```

### `compute_backend`

| Key | Type | Presence |
|---|---|---|
| `kind` | `"gpu"` \| `"cpu"` | Always |
| `device_index` | integer | Only when `kind` is `"gpu"` |
| `device_name` | string | Only when `kind` is `"gpu"` |
| `fallback_reason` | string enum | Only when GPU was requested but `kind` is `"cpu"` |

`fallback_reason` values: `not_compiled`, `no_device`, `invalid_device`,
`init_failed`. Semantics and the operator action each implies are defined in
[../data-model.md](../data-model.md).

## Examples

**Accelerated image, GPU working** — the happy path:

```json
"compute_backend": { "kind": "gpu", "device_index": 0, "device_name": "NVIDIA GeForce RTX 4070" }
```

**Accelerated image, device not passed through to the container** — Story 3's
central scenario:

```json
"compute_backend": { "kind": "cpu", "fallback_reason": "no_device" }
```

**CPU image** — GPU never requested, so no fallback occurred and none is
reported:

```json
"compute_backend": { "kind": "cpu" }
```

**Accelerated image, operator explicitly set `WHISPER_USE_GPU=false`** — a valid
choice, not a failure, so no `fallback_reason` (FR-011):

```json
"compute_backend": { "kind": "cpu" }
```

## Compatibility

Additive only. Existing consumers reading `connected_servers` or `metrics` are
unaffected, and no existing key changes type or meaning.

## Documentation obligation

`src/telemetry/server.rs` serves a Swagger document at `/docs` describing these
endpoints. That document MUST be updated in the same change — an endpoint whose
published schema omits a field it returns is a contract violation, and the
`/docs` route exists precisely so operators do not have to read the source.

## Verification

Covered by [../quickstart.md](../quickstart.md) Scenario 3, which asserts each
`fallback_reason` is reachable and correctly distinguished. Note that a
successful `kind: "gpu"` response can only be produced on a host with a real
GPU — see [../research.md](../research.md) R6 on why that is verified by hand
rather than in CI.
