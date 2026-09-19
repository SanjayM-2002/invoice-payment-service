use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::{auth::Business, error::ApiError, ids::new_id, state::AppState, webhooks};

#[derive(Debug, Deserialize)]
pub struct CreateEndpoint {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct EndpointResponse {
    pub id: String,
    pub url: String,
    pub secret: String,
}

pub async fn create(
    State(state): State<AppState>,
    Business(business_id): Business,
    Json(body): Json<CreateEndpoint>,
) -> Result<(StatusCode, Json<EndpointResponse>), ApiError> {
    let url = body.url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ApiError::invalid("url must be http or https", Some("url")));
    }

    let id = new_id("whe");
    let secret = webhooks::new_secret();

    sqlx::query!(
        "INSERT INTO webhook_endpoints (id, business_id, url, secret)
         VALUES ($1, $2, $3, $4)",
        id,
        business_id,
        url,
        secret,
    )
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(EndpointResponse {
            id,
            url: url.to_string(),
            secret,
        }),
    ))
}
