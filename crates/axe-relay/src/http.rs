use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use serde::Deserialize;
use uuid::Uuid;

use crate::monitor::Monitor;

const DASHBOARD: &str = include_str!("dashboard.html");
const SCRIPT: &str = include_str!("dashboard.js");

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<u64>,
    epoch: Option<Uuid>,
}

pub fn router(monitor: Arc<Monitor>) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/dashboard.js", get(script))
        .route("/api/v1/status", get(status))
        .route("/api/v1/events", get(events))
        .with_state(monitor)
}

async fn status(State(monitor): State<Arc<Monitor>>) -> impl IntoResponse {
    match monitor.snapshot() {
        Some(snapshot) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            axum::Json(snapshot),
        )
            .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "relay is starting").into_response(),
    }
}

async fn events(
    State(monitor): State<Arc<Monitor>>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse {
    let cursor = match (query.epoch, query.after) {
        (None, None) => None,
        (Some(epoch), Some(after)) => Some((epoch, after)),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "epoch and after must be supplied together",
            )
                .into_response();
        }
    };
    match monitor.events(cursor) {
        Ok(batch) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            axum::Json(batch),
        )
            .into_response(),
        Err(reason) => (StatusCode::CONFLICT, reason).into_response(),
    }
}

async fn dashboard() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
                ),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        Html(DASHBOARD),
    )
}

async fn script() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        SCRIPT,
    )
}
