use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
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

pub fn router(monitor: Arc<Monitor>, token: Arc<str>) -> Router {
    let protected = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/events", get(events))
        .route_layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let token = Arc::clone(&token);
                async move {
                    if authorized(&request, &token) {
                        next.run(request).await
                    } else {
                        unauthorized()
                    }
                }
            },
        ));

    Router::new()
        .route("/", get(dashboard))
        .route("/dashboard.js", get(script))
        .merge(protected)
        .with_state(monitor)
}

fn authorized(request: &Request<Body>, expected: &str) -> bool {
    let Some(value) = request.headers().get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("Bearer") && constant_time_eq(token.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
        "authentication required",
    )
        .into_response()
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
