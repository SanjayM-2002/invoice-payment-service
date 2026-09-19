use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};

use crate::{error::ApiError, seed::sha256_hex, state::AppState};


pub struct Business(pub String);

impl FromRequestParts<AppState> for Business {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::InvalidApiKey)?;

        let key = header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::InvalidApiKey)?
            .trim();

        // Hash and check it in api_keys table
        let row = sqlx::query!(
            "SELECT business_id FROM api_keys
             WHERE key_hash = $1 AND revoked_at IS NULL",
            sha256_hex(key)
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::InvalidApiKey)?;

        Ok(Business(row.business_id))
    }
}
