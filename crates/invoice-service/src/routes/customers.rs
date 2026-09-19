use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    auth::Business,
    error::ApiError,
    ids::new_id,
    pagination::{List, Pagination},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct CreateCustomer {
    pub name: String,
    pub email: String,
}

impl CreateCustomer {
    // validator
    fn validate(&self) -> Result<(), ApiError> {
        if self.name.trim().is_empty() || self.name.len() > 255 {
            return Err(ApiError::invalid(
                "name must be between 1 and 255 characters",
                Some("name"),
            ));
        }
        if !self.email.contains('@') || self.email.len() > 255 {
            return Err(ApiError::invalid(
                "email must be a valid address",
                Some("email"),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct CustomerResponse {
    pub id: String,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

// handlers

pub async fn create(
    State(state): State<AppState>,
    Business(business_id): Business, // ← auth. present = protected.
    Json(body): Json<CreateCustomer>,
) -> Result<(StatusCode, Json<CustomerResponse>), ApiError> {
    body.validate()?;

    let row = sqlx::query!(
        "INSERT INTO customers (id, business_id, name, email)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, email, created_at",
        new_id("cus"),
        business_id,
        body.name.trim(),
        body.email.trim(),
    )
    .fetch_one(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CustomerResponse {
            id: row.id,
            name: row.name,
            email: row.email,
            created_at: row.created_at,
        }),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    Business(business_id): Business,
    Path(id): Path<String>,
) -> Result<Json<CustomerResponse>, ApiError> {
    let row = sqlx::query!(
        "SELECT id, name, email, created_at FROM customers
         WHERE id = $1 AND business_id = $2",
        id,
        business_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    Ok(Json(CustomerResponse {
        id: row.id,
        name: row.name,
        email: row.email,
        created_at: row.created_at,
    }))
}

pub async fn list(
    State(state): State<AppState>,
    Business(business_id): Business,
    Query(page): Query<Pagination>,
) -> Result<Json<List<CustomerResponse>>, ApiError> {
    let limit = page.limit();

    let rows = sqlx::query!(
        "SELECT id, name, email, created_at FROM customers
         WHERE business_id = $1
           AND ($2::text IS NULL OR id < $2)
         ORDER BY id DESC
         LIMIT $3",
        business_id,
        page.starting_after,
        limit + 1,
    )
    .fetch_all(&state.db)
    .await?;

    let data = rows
        .into_iter()
        .map(|r| CustomerResponse {
            id: r.id,
            name: r.name,
            email: r.email,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(List::from_rows(data, limit)))
}
