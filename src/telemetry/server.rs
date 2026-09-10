use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
};

use actix_web::{App, HttpResponse, HttpServer, Responder, http::header, web};
use anyhow::Result;
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::{BotState, telemetry::metrics::MetricsSnapshot};

use super::{AppMetrics, ComputeBackend};

#[derive(Clone, Default)]
pub struct InviteTracker {
    inner: Arc<RwLock<Option<String>>>,
}

impl InviteTracker {
    pub fn set(&self, url: String) {
        let mut guard = self.inner.write().expect("invite tracker poisoned");
        *guard = Some(url);
    }

    pub fn get(&self) -> Option<String> {
        self.inner.read().expect("invite tracker poisoned").clone()
    }
}

#[derive(Clone)]
struct HttpAppState {
    bot_state: Arc<BotState>,
    metrics: Arc<AppMetrics>,
    invite: InviteTracker,
}

pub fn spawn_http_server(
    bind_addr: SocketAddr,
    bot_state: Arc<BotState>,
    metrics: Arc<AppMetrics>,
    invite: InviteTracker,
) -> Result<JoinHandle<()>> {
    let server_state = HttpAppState {
        bot_state,
        metrics,
        invite,
    };

    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(server_state.clone()))
            .route("/k8s/readyz", web::get().to(handle_readyz))
            .route("/k8s/livez", web::get().to(handle_livez))
            .route("/k8s/metrics", web::get().to(handle_metrics))
            .route("/invite", web::get().to(handle_invite))
            .route("/docs", web::get().to(swagger_docs))
    })
    .bind(bind_addr)?
    .run();

    Ok(tokio::spawn(async move {
        if let Err(err) = server.await {
            tracing::error!(?err, "HTTP server terminated");
        }
    }))
}

#[derive(Serialize)]
struct ReadyResponse {
    status: &'static str,
    uptime_seconds: u64,
}

async fn handle_readyz(state: web::Data<HttpAppState>) -> impl Responder {
    let snapshot = state.metrics.snapshot();
    HttpResponse::Ok().json(ReadyResponse {
        status: "ok",
        uptime_seconds: snapshot.uptime_seconds,
    })
}

#[derive(Serialize)]
struct LiveResponse {
    status: &'static str,
    connected_servers: usize,
    connected_channels: usize,
    active_participants: usize,
    last_transcription_at: Option<String>,
}

async fn handle_livez(state: web::Data<HttpAppState>) -> impl Responder {
    let snapshot = state.metrics.snapshot();
    HttpResponse::Ok().json(LiveResponse {
        status: "ok",
        connected_servers: state.bot_state.connected_guilds(),
        connected_channels: state.bot_state.connected_channels(),
        active_participants: state.bot_state.active_participants(),
        last_transcription_at: snapshot.last_transcription_at,
    })
}

#[derive(Serialize)]
struct MetricsResponse {
    connected_servers: usize,
    connected_channels: usize,
    active_participants: usize,
    metrics: MetricsSnapshot,
    /// FR-007: the active compute backend, discoverable here rather than only
    /// in startup logs that have long since scrolled away. Sibling of
    /// `metrics`, per contracts/telemetry.md.
    #[serde(skip_serializing_if = "Option::is_none")]
    compute_backend: Option<ComputeBackend>,
}

async fn handle_metrics(state: web::Data<HttpAppState>) -> impl Responder {
    let mut snapshot = state.metrics.snapshot();
    let compute_backend = snapshot.compute_backend.take();
    HttpResponse::Ok().json(MetricsResponse {
        connected_servers: state.bot_state.connected_guilds(),
        connected_channels: state.bot_state.connected_channels(),
        active_participants: state.bot_state.active_participants(),
        metrics: snapshot,
        compute_backend,
    })
}

async fn handle_invite(state: web::Data<HttpAppState>) -> impl Responder {
    if let Some(url) = state.invite.get() {
        HttpResponse::TemporaryRedirect()
            .insert_header((header::LOCATION, url))
            .finish()
    } else {
        HttpResponse::ServiceUnavailable().body("Invite link not available yet")
    }
}

async fn swagger_docs() -> impl Responder {
    HttpResponse::Ok().json(swagger_document())
}

fn swagger_document() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.0.0",
        "info": {
            "title": "Hammock Control Plane",
            "version": "1.0.0",
            "description": "Lightweight endpoints for readiness, liveness, metrics, and invite flow."
        },
        "paths": {
            "/k8s/readyz": {
                "get": {
                    "summary": "Readiness probe",
                    "responses": {
                        "200": {
                            "description": "Service is ready"
                        }
                    }
                }
            },
            "/k8s/livez": {
                "get": {
                    "summary": "Liveness probe",
                    "responses": {
                        "200": {
                            "description": "Service is alive"
                        }
                    }
                }
            },
            "/k8s/metrics": {
                "get": {
                    "summary": "Structured metrics",
                    "responses": {
                        "200": {
                            "description": "JSON metrics payload",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/MetricsResponse" }
                                }
                            }
                        }
                    }
                }
            },
            "/invite": {
                "get": {
                    "summary": "Redirect to Discord invite",
                    "responses": {
                        "307": {
                            "description": "Redirect"
                        }
                    }
                }
            },
            "/docs": {
                "get": {
                    "summary": "OpenAPI specification",
                    "responses": {
                        "200": {
                            "description": "OpenAPI document"
                        }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "MetricsResponse": {
                    "type": "object",
                    "required": ["connected_servers", "connected_channels", "active_participants", "metrics", "compute_backend"],
                    "properties": {
                        "connected_servers": { "type": "integer" },
                        "connected_channels": { "type": "integer" },
                        "active_participants": { "type": "integer" },
                        "metrics": { "$ref": "#/components/schemas/Metrics" },
                        "compute_backend": { "$ref": "#/components/schemas/ComputeBackend" }
                    }
                },
                "Metrics": {
                    "type": "object",
                    "description": "Transcription totals, throughput and overload signals.",
                    "required": [
                        "uptime_seconds", "total_transcribed_lines", "total_sessions_started",
                        "total_sessions_completed", "total_transcription_errors",
                        "total_utterances_discarded", "transcription_queue_depth",
                        "transcription_in_flight", "transcription_concurrency_limit",
                        "transcription_duration_ms", "total_segments_rejected_as_non_speech",
                        "utterance_length_ms", "line_windows"
                    ],
                    "properties": {
                        "uptime_seconds": { "type": "integer" },
                        "total_transcribed_lines": { "type": "integer" },
                        "total_sessions_started": { "type": "integer" },
                        "total_sessions_completed": { "type": "integer" },
                        "total_transcription_errors": {
                            "type": "integer",
                            "description": "Transcriptions that failed after startup. A rising count with a healthy compute_backend means the device went away after the bot bound it."
                        },
                        "total_utterances_discarded": {
                            "type": "integer",
                            "description": "Utterances dropped because the transcription queue was full. Non-zero means work was lost, not merely delayed."
                        },
                        "transcription_queue_depth": {
                            "type": "integer",
                            "description": "Jobs enqueued and not yet picked up by a worker. Rising across polls means the system is falling behind."
                        },
                        "transcription_in_flight": {
                            "type": "integer",
                            "description": "Jobs currently decoding. Never exceeds transcription_concurrency_limit. Zero while queue_depth is non-zero means the worker pool is wedged, which is a defect rather than overload."
                        },
                        "transcription_concurrency_limit": {
                            "type": "integer",
                            "description": "Resolved worker count, from TRANSCRIPTION_CONCURRENCY or the per-backend default. Constant for the process lifetime."
                        },
                        "transcription_duration_ms": { "$ref": "#/components/schemas/DurationStats" },
                        "total_segments_rejected_as_non_speech": {
                            "type": "integer",
                            "description": "Segments transcribed but producing no caption, because the audio contained no speech. NOT the same as total_utterances_discarded: a discard means the queue was full and work was lost; a rejection means there was nothing to caption and nothing was lost. Sustained growth usually means an open microphone in a noisy room."
                        },
                        "utterance_length_ms": {
                            "allOf": [{ "$ref": "#/components/schemas/DurationStats" }],
                            "description": "How long produced utterances were, in audio time — not how long they took to transcribe. A p50 collapsing toward UTTERANCE_MIN_SPEECH_MS means the silence threshold is too low and utterances are fragmenting; a p50 pinned at UTTERANCE_MAX_SECS means boundaries are coming from the length cap rather than from speech."
                        },
                        "last_transcription_at": { "type": "string", "format": "date-time", "nullable": true },
                        "line_windows": {
                            "type": "object",
                            "description": "Transcribed line counts over rolling windows (1h/30m/15m/5m/1m/30s)."
                        }
                    }
                },
                "DurationStats": {
                    "type": "object",
                    "description":
                        "Transcription wall time. `count` and `total_ms` are lifetime totals; the                          percentiles are over a bounded recent window, because a p95 polluted by a                          slow decode an hour ago says nothing about whether the system is keeping up now.",
                    "required": ["count", "total_ms", "p50_ms", "p95_ms", "max_ms"],
                    "properties": {
                        "count": { "type": "integer", "description": "Completed transcriptions measured (lifetime)." },
                        "total_ms": { "type": "integer", "description": "Sum of decode time (lifetime), so a mean is derivable." },
                        "p50_ms": { "type": "integer", "description": "Median over the recent window." },
                        "p95_ms": { "type": "integer", "description": "95th percentile over the recent window." },
                        "max_ms": { "type": "integer", "description": "Largest in the recent window." }
                    }
                },
                "ComputeBackend": {
                    "type": "object",
                    "description":
                        "Which processor transcription is running on, resolved once at startup                          and immutable thereafter. When GPU was requested but could not be used,                          `kind` is `cpu` and `fallback_reason` says which of the four causes it was.",
                    "required": ["kind"],
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["gpu", "cpu"],
                            "description": "The processor executing transcription."
                        },
                        "device_index": {
                            "type": "integer",
                            "description": "The bound GPU device index. Present only when kind is gpu."
                        },
                        "device_name": {
                            "type": "string",
                            "description": "Device identity as reported by the native library. Present only when kind is gpu.",
                            "example": "NVIDIA GeForce RTX 4070"
                        },
                        "fallback_reason": {
                            "type": "string",
                            "enum": ["not_compiled", "no_device", "invalid_device", "init_failed"],
                            "description":
                                "Why GPU was requested but not used. Absent when the backend is what                                  was requested — including when the operator explicitly disabled GPU,                                  which is a valid choice rather than a fallback."
                        }
                    }
                }
            }
        }
    })
}
