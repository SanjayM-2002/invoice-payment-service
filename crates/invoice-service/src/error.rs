use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};

use crate::request_id;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{message}")]
    InvalidRequest {
        message: String,
        param: Option<String>,
    },

    #[error("An Idempotency-Key header is required for this endpoint")]
    MissingIdempotencyKey,

    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("Resource not found")]
    NotFound,

    #[error("Invoice is {current_state} and cannot be paid")]
    InvoiceNotPayable { current_state: String },

    #[error("Invoice is {current_state} and cannot be moved to {attempted_state}")]
    InvalidTransition {
        current_state: String,
        attempted_state: String,
    },

    #[error("Another payment attempt is already in progress for this invoice")]
    PaymentAlreadyInProgress { attempt_id: String },

    #[error("A request with this Idempotency-Key is already in progress")]
    RequestInProgress,

    #[error("This Idempotency-Key was already used with a different request")]
    IdempotencyKeyReuse,

    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn invalid(message: impl Into<String>, param: Option<&str>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
            param: param.map(str::to_string),
        }
    }

    /// HTTP status + code
    fn parts(&self) -> (StatusCode, &'static str) {
        use ApiError::*;
        match self {
            InvalidRequest { .. }         => (StatusCode::BAD_REQUEST, "invalid_request"),
            MissingIdempotencyKey         => (StatusCode::BAD_REQUEST, "missing_idempotency_key"),
            InvalidApiKey                 => (StatusCode::UNAUTHORIZED, "invalid_api_key"),
            NotFound                      => (StatusCode::NOT_FOUND, "not_found"),
            InvoiceNotPayable { .. }      => (StatusCode::CONFLICT, "invoice_not_payable"),
            InvalidTransition { .. }      => (StatusCode::CONFLICT, "invalid_transition"),
            PaymentAlreadyInProgress { .. } => (StatusCode::CONFLICT, "payment_already_in_progress"),
            RequestInProgress             => (StatusCode::CONFLICT, "request_in_progress"),
            IdempotencyKeyReuse           => (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reuse"),
            Database(_) | Internal(_)     => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }

    /// Context fields that ride alongside code and message for some errors.
    fn extra(&self) -> Map<String, Value> {
        use ApiError::*;
        let mut m = Map::new();
        match self {
            InvalidRequest { param: Some(p), .. } => {
                m.insert("param".into(), json!(p));
            }
            InvoiceNotPayable { current_state } => {
                m.insert("current_state".into(), json!(current_state));
            }
            InvalidTransition { current_state, attempted_state } => {
                m.insert("current_state".into(), json!(current_state));
                m.insert("attempted_state".into(), json!(attempted_state));
            }
            PaymentAlreadyInProgress { attempt_id } => {
                m.insert("attempt_id".into(), json!(attempt_id));
            }
            _ => {}
        }
        m
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();

        // Log the real cause for 5xx, but will not send to client.
        if status.is_server_error() {
            tracing::error!(error = ?self, code, "request failed");
        }

        let message = if status.is_server_error() {
            "An unexpected error occurred.".to_string()
        } else {
            self.to_string()
        };

        let mut error = Map::new();
        error.insert("code".into(), json!(code));
        error.insert("message".into(), json!(message));
        error.insert("request_id".into(), json!(request_id::current()));
        error.extend(self.extra());

        (status, Json(json!({ "error": Value::Object(error) }))).into_response()
    }
}
