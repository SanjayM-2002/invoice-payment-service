pub mod auth;
pub mod config;
pub mod error;
pub mod idempotency;
pub mod ids;
pub mod money;
pub mod pagination;
pub mod psp;
pub mod request_id;
pub mod routes;
pub mod seed;
pub mod state;
pub mod state_machine;
pub mod webhooks;
pub mod workers;

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use tower_http::trace::TraceLayer;

use crate::{error::ApiError, state::AppState};

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(routes::router())
        .layer(axum::middleware::from_fn(request_id::middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness + database reachability
async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}
