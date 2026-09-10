//! The compute backend transcription is actually running on, and why.
//!
//! Resolved exactly once, when the transcription worker is constructed, and
//! immutable for the process lifetime. Nothing here is evaluated on the voice
//! tick path or per utterance — Constitution Principle I depends on that, and
//! the resolution point is what guarantees it.
//!
//! Two values live here:
//!
//! - [`ComputeBackend`] — what is in use, exposed on `/k8s/metrics` per
//!   `specs/001-gpu-accelerated-builds/contracts/telemetry.md`.
//! - [`FallbackReason`] — why GPU was asked for and not delivered, classified
//!   into the four causes FR-008 requires an operator be able to tell apart.

use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// Which processor transcription runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Gpu,
    Cpu,
}

/// Why GPU was requested but CPU is in use.
///
/// Every value maps to exactly one thing the operator should do about it. That
/// mapping is the point of the enum: a single "GPU unavailable" warning would
/// fail FR-008 and leave the operator guessing between four unrelated fixes.
///
/// Note what is deliberately absent: there is no `disabled_by_config`. An
/// operator who turns GPU off has not experienced a fallback, and FR-011
/// requires that choice be honored without a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// Running a CPU build. The `cuda` feature was not compiled in.
    NotCompiled,
    /// No GPU is visible to this process.
    NoDevice,
    /// The configured device index does not exist.
    InvalidDevice,
    /// A device is present but the native context would not initialize on it.
    InitFailed,
}

impl FallbackReason {
    /// The short machine-readable form, matching the telemetry contract.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotCompiled => "not_compiled",
            Self::NoDevice => "no_device",
            Self::InvalidDevice => "invalid_device",
            Self::InitFailed => "init_failed",
        }
    }

    /// What the operator should actually do. This is the sentence that decides
    /// whether SC-004 holds — whether someone can fix their configuration from
    /// the bot's own output without reaching for external tooling.
    pub fn operator_action(self) -> &'static str {
        match self {
            Self::NotCompiled => {
                "pull the accelerated image variant (the `cuda-runtime-*` tags); \
                 this image is a CPU build and has no GPU support compiled in"
            }
            Self::NoDevice => {
                "grant the container access to a GPU — device passthrough is most \
                 likely missing (`--gpus all`, or the `deploy.resources.reservations.devices` \
                 block in compose.yml), and the host needs the NVIDIA Container Toolkit"
            }
            Self::InvalidDevice => "correct WHISPER_GPU_DEVICE to a device index that exists",
            Self::InitFailed => {
                "read the initialization error above — it is usually a driver \
                 mismatch or insufficient GPU memory for the configured model"
            }
        }
    }
}

/// The processor actually executing transcription, together with why it is what
/// it is.
///
/// Constructed only through the three constructors below, which is how the
/// validation rules in `data-model.md` are enforced: there is no way to build a
/// CPU backend carrying a device name, or a GPU backend carrying a fallback
/// reason, because no constructor produces one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComputeBackend {
    kind: BackendKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback_reason: Option<FallbackReason>,
}

impl ComputeBackend {
    /// GPU is in use, on this device.
    pub fn gpu(device_index: i32, device_name: Option<String>) -> Self {
        Self {
            kind: BackendKind::Gpu,
            device_index: Some(device_index),
            device_name,
            fallback_reason: None,
        }
    }

    /// CPU is in use and that is what was asked for — either a CPU build, or an
    /// operator who explicitly set `WHISPER_USE_GPU=false`. Not a fallback, so
    /// no reason is recorded and nothing warns (FR-011).
    pub fn cpu() -> Self {
        Self {
            kind: BackendKind::Cpu,
            device_index: None,
            device_name: None,
            fallback_reason: None,
        }
    }

    /// GPU was requested and could not be delivered. Runs on CPU (FR-009), and
    /// records which of the four causes it was (FR-008).
    pub fn cpu_fallback(reason: FallbackReason) -> Self {
        Self {
            kind: BackendKind::Cpu,
            device_index: None,
            device_name: None,
            fallback_reason: Some(reason),
        }
    }

    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    pub fn device_index(&self) -> Option<i32> {
        self.device_index
    }

    pub fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    pub fn fallback_reason(&self) -> Option<FallbackReason> {
        self.fallback_reason
    }

    /// A one-line human description, used in the startup log (FR-006).
    pub fn describe(&self) -> String {
        match self.kind {
            BackendKind::Gpu => match (&self.device_name, self.device_index) {
                (Some(name), Some(index)) => format!("GPU (device {index}: {name})"),
                (None, Some(index)) => format!("GPU (device {index})"),
                _ => "GPU".to_string(),
            },
            BackendKind::Cpu => "CPU".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// ggml device discovery
// ---------------------------------------------------------------------------

/// What ggml told us about the GPUs it can see.
///
/// ggml reports device discovery through the log callback and nowhere else —
/// there is no query API on the whisper-rs surface. So the log forwarder is the
/// only seam available, and capturing it here is what lets `no_device` be told
/// apart from `init_failed`: without it, both look identical from the outside,
/// which is precisely the ambiguity FR-008 forbids. See research.md R4.
#[derive(Debug, Default, Clone)]
pub struct GpuDiscovery {
    /// `None` means ggml never said — which, for a CUDA build, means it never
    /// got as far as enumerating.
    pub device_count: Option<usize>,
    /// Names by device index, in the order ggml reported them.
    pub device_names: Vec<(usize, String)>,
    /// ggml reported an explicit CUDA initialization failure.
    pub init_error: Option<String>,
}

impl GpuDiscovery {
    pub fn name_for(&self, index: i32) -> Option<String> {
        if index < 0 {
            return None;
        }
        self.device_names
            .iter()
            .find(|(idx, _)| *idx == index as usize)
            .map(|(_, name)| name.clone())
    }
}

fn discovery() -> &'static Mutex<GpuDiscovery> {
    static DISCOVERY: OnceLock<Mutex<GpuDiscovery>> = OnceLock::new();
    DISCOVERY.get_or_init(|| Mutex::new(GpuDiscovery::default()))
}

/// Feed one ggml log line into discovery state.
///
/// Called from the whisper log forwarder in `transcription.rs` for every line
/// ggml emits, before the line is re-emitted as a `debug!`. Parsing is
/// tolerant: an unrecognized line is simply not interesting, never an error.
pub fn observe_ggml_log(message: &str) {
    let Some(update) = parse_ggml_log(message) else {
        return;
    };

    let mut state = discovery().lock().unwrap_or_else(|err| err.into_inner());
    match update {
        DiscoveryUpdate::DeviceCount(count) => state.device_count = Some(count),
        DiscoveryUpdate::Device { index, name } => {
            state.device_names.retain(|(idx, _)| *idx != index);
            state.device_names.push((index, name));
            state.device_names.sort_by_key(|(idx, _)| *idx);
        }
        DiscoveryUpdate::InitError(err) => state.init_error = Some(err),
    }
}

/// A copy of what ggml has reported so far.
pub fn gpu_discovery() -> GpuDiscovery {
    discovery()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

#[derive(Debug, PartialEq, Eq)]
enum DiscoveryUpdate {
    DeviceCount(usize),
    Device { index: usize, name: String },
    InitError(String),
}

/// Pull device facts out of a ggml log line.
///
/// The lines this recognizes look like:
///
/// ```text
/// ggml_cuda_init: found 2 CUDA devices:
///   Device 0: NVIDIA GeForce RTX 3090 Ti, compute capability 8.6, VMM: yes
///   Device 1: NVIDIA GeForce GTX 1080, compute capability 6.1, VMM: yes
/// ggml_cuda_init: failed to initialize CUDA: no CUDA-capable device is detected
/// ```
fn parse_ggml_log(message: &str) -> Option<DiscoveryUpdate> {
    let trimmed = message.trim();

    if let Some(rest) = trimmed.split("failed to initialize CUDA:").nth(1) {
        return Some(DiscoveryUpdate::InitError(rest.trim().to_string()));
    }

    if trimmed.contains("CUDA devices")
        && let Some(after) = trimmed.split("found ").nth(1)
        && let Some(count) = after.split_whitespace().next()
        && let Ok(count) = count.parse::<usize>()
    {
        return Some(DiscoveryUpdate::DeviceCount(count));
    }

    if let Some(rest) = trimmed.strip_prefix("Device ")
        && let Some((index, remainder)) = rest.split_once(':')
        && let Ok(index) = index.trim().parse::<usize>()
    {
        // The name runs to the first comma; everything after it is compute
        // capability and VMM flags, which are not identity.
        let name = remainder
            .split(',')
            .next()
            .unwrap_or(remainder)
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(DiscoveryUpdate::Device { index, name });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_backend_carries_no_device_identity() {
        let backend = ComputeBackend::cpu();
        assert_eq!(backend.device_index(), None);
        assert_eq!(backend.device_name(), None);
        assert_eq!(backend.fallback_reason(), None);
    }

    #[test]
    fn a_deliberate_opt_out_is_not_a_fallback() {
        // FR-011: an operator setting WHISPER_USE_GPU=false made a valid
        // choice. The row most easily got wrong.
        assert_eq!(ComputeBackend::cpu().fallback_reason(), None);
    }

    #[test]
    fn serialization_matches_the_telemetry_contract() {
        let gpu = ComputeBackend::gpu(0, Some("NVIDIA GeForce RTX 4070".to_string()));
        assert_eq!(
            serde_json::to_value(&gpu).unwrap(),
            serde_json::json!({
                "kind": "gpu",
                "device_index": 0,
                "device_name": "NVIDIA GeForce RTX 4070",
            })
        );

        assert_eq!(
            serde_json::to_value(ComputeBackend::cpu()).unwrap(),
            serde_json::json!({ "kind": "cpu" })
        );

        assert_eq!(
            serde_json::to_value(ComputeBackend::cpu_fallback(FallbackReason::NoDevice)).unwrap(),
            serde_json::json!({ "kind": "cpu", "fallback_reason": "no_device" })
        );
    }

    #[test]
    fn every_fallback_reason_has_a_distinct_action() {
        let reasons = [
            FallbackReason::NotCompiled,
            FallbackReason::NoDevice,
            FallbackReason::InvalidDevice,
            FallbackReason::InitFailed,
        ];
        let mut actions: Vec<&str> = reasons.iter().map(|r| r.operator_action()).collect();
        actions.sort_unstable();
        actions.dedup();
        assert_eq!(
            actions.len(),
            reasons.len(),
            "FR-008 requires four distinguishable causes"
        );
    }

    #[test]
    fn parses_the_ggml_device_banner() {
        assert_eq!(
            parse_ggml_log("ggml_cuda_init: found 2 CUDA devices:"),
            Some(DiscoveryUpdate::DeviceCount(2))
        );
        assert_eq!(
            parse_ggml_log(
                "  Device 0: NVIDIA GeForce RTX 3090 Ti, compute capability 8.6, VMM: yes"
            ),
            Some(DiscoveryUpdate::Device {
                index: 0,
                name: "NVIDIA GeForce RTX 3090 Ti".to_string()
            })
        );
        assert_eq!(
            parse_ggml_log(
                "ggml_cuda_init: failed to initialize CUDA: no CUDA-capable device is detected"
            ),
            Some(DiscoveryUpdate::InitError(
                "no CUDA-capable device is detected".to_string()
            ))
        );
    }

    #[test]
    fn ignores_lines_that_say_nothing_about_devices() {
        assert_eq!(
            parse_ggml_log("whisper_init_with_params_no_state: use gpu = 1"),
            None
        );
        assert_eq!(parse_ggml_log(""), None);
        assert_eq!(parse_ggml_log("Device zero: something"), None);
    }

    #[test]
    fn device_names_are_looked_up_by_index() {
        let discovery = GpuDiscovery {
            device_count: Some(2),
            device_names: vec![(0, "A".into()), (1, "B".into())],
            init_error: None,
        };
        assert_eq!(discovery.name_for(1).as_deref(), Some("B"));
        assert_eq!(discovery.name_for(7), None);
        assert_eq!(discovery.name_for(-1), None);
    }

    #[test]
    fn describes_itself_for_the_startup_log() {
        assert_eq!(
            ComputeBackend::gpu(1, Some("NVIDIA A10".into())).describe(),
            "GPU (device 1: NVIDIA A10)"
        );
        assert_eq!(ComputeBackend::cpu().describe(), "CPU");
    }
}
