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
    money::{self, MAX_LINE_ITEMS, MAX_QUANTITY, MAX_UNIT_AMOUNT_CENTS},
    pagination::{List, Pagination},
    state::AppState,
    state_machine::InvoiceStatus,
    webhooks,
};


#[derive(Debug, Deserialize)]
pub struct CreateInvoice {
    pub customer_id: String,
    pub due_date: DateTime<Utc>,
    pub line_items: Vec<LineItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct LineItemInput {
    pub description: String,
    pub quantity: i32,
    pub unit_amount_cents: i64,
}

impl CreateInvoice {
    fn validate(&self) -> Result<(), ApiError> {
        if self.line_items.is_empty() || self.line_items.len() > MAX_LINE_ITEMS {
            return Err(ApiError::invalid(
                format!("line_items must contain between 1 and {MAX_LINE_ITEMS} items"),
                Some("line_items"),
            ));
        }
        for item in &self.line_items {
            let d = item.description.trim();
            if d.is_empty() || d.len() > 500 {
                return Err(ApiError::invalid(
                    "description must be between 1 and 500 characters",
                    Some("line_items.description"),
                ));
            }
            if !(1..=MAX_QUANTITY).contains(&item.quantity) {
                return Err(ApiError::invalid(
                    format!("quantity must be between 1 and {MAX_QUANTITY}"),
                    Some("line_items.quantity"),
                ));
            }
            if !(0..=MAX_UNIT_AMOUNT_CENTS).contains(&item.unit_amount_cents) {
                return Err(ApiError::invalid(
                    format!("unit_amount_cents must be between 0 and {MAX_UNIT_AMOUNT_CENTS}"),
                    Some("line_items.unit_amount_cents"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    #[serde(flatten)]
    pub page: Pagination,
}


#[derive(Debug, Serialize)]
pub struct InvoiceResponse {
    pub id: String,
    pub customer_id: String,
    pub status: InvoiceStatus,
    pub total_cents: i64,
    pub currency: String,
    pub due_date: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub line_items: Vec<LineItemResponse>,
    pub payment_attempts: Vec<PaymentAttemptResponse>,
}

#[derive(Debug, Serialize)]
pub struct LineItemResponse {
    pub id: String,
    pub description: String,
    pub quantity: i32,
    pub unit_amount_cents: i64,
    pub amount_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct PaymentAttemptResponse {
    pub id: String,
    pub status: String,
    pub amount_cents: i64,
    pub psp_ref: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}


pub async fn create(
    State(state): State<AppState>,
    Business(business_id): Business,
    Json(body): Json<CreateInvoice>,
) -> Result<(StatusCode, Json<InvoiceResponse>), ApiError> {
    body.validate()?;

    // The server computes the total
    let pairs: Vec<(i32, i64)> = body
        .line_items
        .iter()
        .map(|i| (i.quantity, i.unit_amount_cents))
        .collect();
    let total_cents = money::invoice_total(&pairs)?;

    // Ensuring business sends its own customer_id
    sqlx::query!(
        "SELECT id FROM customers WHERE id = $1 AND business_id = $2",
        body.customer_id,
        business_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::invalid("customer_id not found", Some("customer_id")))?;

    let invoice_id = new_id("inv");

    let mut tx = state.db.begin().await?;

    let invoice = sqlx::query!(
        "INSERT INTO invoices (id, business_id, customer_id, total_cents, due_date)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, customer_id, status, total_cents, currency, due_date, created_at",
        invoice_id,
        business_id,
        body.customer_id,
        total_cents,
        body.due_date,
    )
    .fetch_one(&mut *tx)
    .await?;

    let mut line_items = Vec::with_capacity(body.line_items.len());
    for (index, item) in body.line_items.iter().enumerate() {
        let row = sqlx::query!(
            "INSERT INTO line_items
                 (id, invoice_id, description, quantity, unit_amount_cents, sort_order)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, description, quantity, unit_amount_cents",
            new_id("li"),
            invoice_id,
            item.description.trim(),
            item.quantity,
            item.unit_amount_cents,
            index as i32,
        )
        .fetch_one(&mut *tx)
        .await?;

        line_items.push(LineItemResponse {
            amount_cents: money::line_amount(row.quantity, row.unit_amount_cents)?,
            id: row.id,
            description: row.description,
            quantity: row.quantity,
            unit_amount_cents: row.unit_amount_cents,
        });
    }

    // Outbox: the event is written in the SAME transaction as the invoice.
    // There is no moment where the invoice exists and the notification doesn't.
    webhooks::emit_invoice_event(&mut tx, &business_id, "invoice.created", &invoice_id).await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(InvoiceResponse {
            id: invoice.id,
            customer_id: invoice.customer_id,
            status: InvoiceStatus::from_db(&invoice.status),
            total_cents: invoice.total_cents,
            currency: invoice.currency,
            due_date: invoice.due_date,
            created_at: invoice.created_at,
            line_items,
            payment_attempts: vec![],
        }),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    Business(business_id): Business,
    Path(id): Path<String>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    Ok(Json(load(&state, &business_id, &id).await?))
}

pub async fn list(
    State(state): State<AppState>,
    Business(business_id): Business,
    Query(query): Query<ListQuery>,
) -> Result<Json<List<InvoiceResponse>>, ApiError> {
    let limit = query.page.limit();

    // Validate the filter rather than passing an arbitrary string to SQL.
    if let Some(status) = &query.status {
        if !matches!(
            status.as_str(),
            "open" | "paid" | "void" | "uncollectible"
        ) {
            return Err(ApiError::invalid("unknown status", Some("status")));
        }
    }

    let rows = sqlx::query!(
        "SELECT id FROM invoices
         WHERE business_id = $1
           AND ($2::text IS NULL OR status = $2)
           AND ($3::text IS NULL OR id < $3)
         ORDER BY id DESC
         LIMIT $4",
        business_id,
        query.status,
        query.page.starting_after,
        limit + 1,
    )
    .fetch_all(&state.db)
    .await?;

    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        data.push(load(&state, &business_id, &row.id).await?);
    }

    Ok(Json(List::from_rows(data, limit)))
}

pub async fn void(
    state: State<AppState>,
    business: Business,
    path: Path<String>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    transition(state, business, path, InvoiceStatus::Void).await
}

pub async fn uncollectible(
    state: State<AppState>,
    business: Business,
    path: Path<String>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    transition(state, business, path, InvoiceStatus::Uncollectible).await
}

// helpers 


async fn transition(
    State(state): State<AppState>,
    Business(business_id): Business,
    Path(id): Path<String>,
    target: InvoiceStatus,
) -> Result<Json<InvoiceResponse>, ApiError> {
    let current = sqlx::query!(
        "SELECT status FROM invoices WHERE id = $1 AND business_id = $2",
        id,
        business_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    // Rejected before anything is written.
    InvoiceStatus::from_db(&current.status).ensure_can_transition_to(target)?;

    sqlx::query!(
        "UPDATE invoices SET status = $1, updated_at = now()
         WHERE id = $2 AND business_id = $3",
        target.as_str(),
        id,
        business_id
    )
    .execute(&state.db)
    .await?;

    Ok(Json(load(&state, &business_id, &id).await?))
}


/// Common assembler for full invoice shape
async fn load(
    state: &AppState,
    business_id: &str,
    id: &str,
) -> Result<InvoiceResponse, ApiError> {
    let invoice = sqlx::query!(
        "SELECT id, customer_id, status, total_cents, currency, due_date, created_at
         FROM invoices WHERE id = $1 AND business_id = $2",
        id,
        business_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let line_rows = sqlx::query!(
        "SELECT id, description, quantity, unit_amount_cents
         FROM line_items WHERE invoice_id = $1 ORDER BY sort_order",
        id
    )
    .fetch_all(&state.db)
    .await?;

    let mut line_items = Vec::with_capacity(line_rows.len());
    for row in line_rows {
        line_items.push(LineItemResponse {
            amount_cents: money::line_amount(row.quantity, row.unit_amount_cents)?,
            id: row.id,
            description: row.description,
            quantity: row.quantity,
            unit_amount_cents: row.unit_amount_cents,
        });
    }

    let attempts = sqlx::query!(
        "SELECT id, status, amount_cents, psp_ref, failure_code, created_at, resolved_at
         FROM payment_attempts WHERE invoice_id = $1 ORDER BY created_at",
        id
    )
    .fetch_all(&state.db)
    .await?;

    Ok(InvoiceResponse {
        id: invoice.id,
        customer_id: invoice.customer_id,
        status: InvoiceStatus::from_db(&invoice.status),
        total_cents: invoice.total_cents,
        currency: invoice.currency,
        due_date: invoice.due_date,
        created_at: invoice.created_at,
        line_items,
        payment_attempts: attempts
            .into_iter()
            .map(|a| PaymentAttemptResponse {
                id: a.id,
                status: a.status,
                amount_cents: a.amount_cents,
                psp_ref: a.psp_ref,
                failure_code: a.failure_code,
                created_at: a.created_at,
                resolved_at: a.resolved_at,
            })
            .collect(),
    })
}
