pub mod customers;
pub mod invoices;
pub mod payments;
pub mod webhook_endpoints;

use axum::{
    routing::{get, post},
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/customers", post(customers::create).get(customers::list))
        .route("/v1/customers/{id}", get(customers::get))
        .route("/v1/webhook_endpoints", post(webhook_endpoints::create))
        .route("/v1/invoices", post(invoices::create).get(invoices::list))
        .route("/v1/invoices/{id}", get(invoices::get))
        .route("/v1/invoices/{id}/pay", post(payments::pay))
        .route("/v1/invoices/{id}/void", post(invoices::void))
        .route("/v1/invoices/{id}/uncollectible", post(invoices::uncollectible))
}
