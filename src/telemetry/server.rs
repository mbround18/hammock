use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
};

use actix_web::{App, HttpResponse, HttpServer, Responder, http::header, web};
use anyhow::Result;
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::{BotState, telemetry::metrics::MetricsSnapshot};

use super::AppMetrics;

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
}

async fn handle_metrics(state: web::Data<HttpAppState>) -> impl Responder {
    let snapshot = state.metrics.snapshot();
    HttpResponse::Ok().json(MetricsResponse {
        connected_servers: state.bot_state.connected_guilds(),
        connected_channels: state.bot_state.connected_channels(),
        active_participants: state.bot_state.active_participants(),
        metrics: snapshot,
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
                            "description": "JSON metrics payload"
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
        }
    })
}
