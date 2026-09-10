# Contract: TRANSCRIPTION_CONCURRENCY

**Feature**: [../spec.md](../spec.md) | Satisfies **FR-011**, **FR-012**

The one new tunable this feature introduces. Constitution Principle IV governs
its shape: a documented default safe for a CPU-only self-hoster, an entry in
`.env.sample` explaining its effect, and validation at startup with an actionable
error rather than a failure later at use.

## Variable

`TRANSCRIPTION_CONCURRENCY` — how many utterances may be transcribed at once.

| | |
|---|---|
| Type | positive integer |
| Accepted range | `1` to `32` inclusive |
| Default | derived from the active compute backend, below |
| Validated | at startup, before the worker pool is built |

## Defaults

The default depends on the resolved `ComputeBackend` from feature 001, which is
why the pool is constructed after backend resolution.

| Backend | Default | Reasoning |
|---|---|---|
| CPU | `clamp(available_parallelism / 4, 1, 4)` | See the oversubscription note below. |
| GPU | `2` | Bounded by device memory per decoder state, not by core count. |

### Why the CPU default divides by four

This is the part that is easy to get wrong, so it is written down.

whisper.cpp already parallelises a **single** decode internally:
`whisper_full_default_params` sets `n_threads = min(4, hardware_concurrency)`
(`whisper.cpp:5927`). So each worker is already using up to 4 threads. Running
one worker per core would oversubscribe the machine roughly fourfold, and the
result is slower than doing less — context switching against a workload that was
already saturating its cores.

Dividing by four keeps total thread demand near the core count. The upper clamp
of 4 exists because throughput gains flatten well before that on realistic
channel sizes, while each additional worker still costs memory.

On this 32-core development host that yields a default of 4.

### Why the GPU default is a small constant

Core count is irrelevant on GPU; memory is the binding constraint. Each decoder
state costs ~233 MB for `base` on CUDA above a shared 147 MB model, and those
buffers scale with model size, so a `large` model costs several times that per
state (research R4).

`2` is enough to overlap one decode with the next job's setup — which is where
the serialization stall actually shows up — while staying safe on a small card
with a large model. An operator with a big GPU raises it; that is what the
variable is for.

## Validation

Checked at startup, before any state is allocated. Each failure names the value
received and the accepted range (FR-011, Principle IV):

| Input | Behavior |
|---|---|
| unset or empty | Use the backend default. Report it and its source. |
| a valid integer in `1..=32` | Use it. |
| `0` | **Refuse to start.** `TRANSCRIPTION_CONCURRENCY=0 is not valid: transcription would never run. Accepted range is 1 to 32.` |
| negative, or not an integer | **Refuse to start.** `TRANSCRIPTION_CONCURRENCY='abc' is not a positive integer. Accepted range is 1 to 32.` |
| above `32` | **Refuse to start.** `TRANSCRIPTION_CONCURRENCY=64 exceeds the maximum of 32.` |

**These refuse to start, unlike feature 001's GPU settings, and the difference is
deliberate.** A GPU that is absent is an environment problem the operator may not
control, so Principle III requires degrading to CPU. A concurrency value that is
nonsense is a configuration problem the operator does control and can fix in
seconds — and silently substituting a value would leave them believing a setting
took effect when it did not, which FR-011's "rather than failing later or
silently substituting a value" rules out explicitly.

Note the asymmetry with the **memory** failure mode: a value that is valid but
too large for the accelerator is not detectable by range-checking. It surfaces
when the Nth state fails to allocate, which is why states are created eagerly at
startup (see [../data-model.md](../data-model.md)) — so it fails at startup with
a message naming the constraint, rather than mid-conversation.

## Startup report (FR-012)

The effective limit is logged at startup, next to feature 001's backend line:

```text
INFO transcription backend: GPU (device 0: NVIDIA GeForce RTX 4070)
INFO transcription concurrency: 2 workers (default for gpu backend)
```

```text
INFO transcription backend: CPU
INFO transcription concurrency: 4 workers (default for cpu backend, 32 cores available)
```

```text
INFO transcription concurrency: 8 workers (from TRANSCRIPTION_CONCURRENCY)
```

The source is included because "4 workers" alone does not tell an operator
whether their configuration was read. The same value is exposed as
`transcription_concurrency_limit` on `/k8s/metrics`, since startup output
scrolls away — the same reasoning feature 001 applied to the compute backend.

## `.env.sample`

Documented alongside the GPU settings, with the effect of raising and lowering
it, the per-backend defaults, and a pointer to the metrics that show whether the
current value is right.
