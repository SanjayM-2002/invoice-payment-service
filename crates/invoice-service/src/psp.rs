use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Waiting time for PSP
const TIMEOUT: Duration = Duration::from_secs(5);


#[derive(Debug)]
pub enum ChargeOutcome {
    Succeeded { psp_ref: String },
    Failed { code: String },
    Unknown,
}

/// Information about a charge we lost track of
#[derive(Debug)]
pub enum ChargeLookup {
    InProgress,
    Succeeded { psp_ref: String },
    Failed { code: String },
    /// Never seen this key
    NotFound,
    Unreachable,
}

#[derive(Serialize)]
struct ChargeRequest<'a> {
    idempotency_key: &'a str,
    amount_cents: i64,
    card_token: &'a str,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PspResponse {
    InProgress,
    Succeeded { psp_ref: String },
    Failed { code: String },
}

#[derive(Clone)]
pub struct PspClient {
    http: reqwest::Client,
    base_url: String,
}

impl PspClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("failed to build http client");
        Self { http, base_url }
    }

    /// idempotency_key => attempt id
    pub async fn charge(
        &self,
        idempotency_key: &str,
        amount_cents: i64,
        card_token: &str,
    ) -> ChargeOutcome {
        let result = self
            .http
            .post(format!("{}/charges", self.base_url))
            .json(&ChargeRequest {
                idempotency_key,
                amount_cents,
                card_token,
            })
            .send()
            .await;

        let response = match result {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), key = idempotency_key, "psp returned error status");
                return ChargeOutcome::Unknown;
            }
            Err(e) => {
                tracing::warn!(error = %e, key = idempotency_key, "psp unreachable or timed out");
                return ChargeOutcome::Unknown;
            }
        };

        match response.json::<PspResponse>().await {
            Ok(PspResponse::Succeeded { psp_ref }) => ChargeOutcome::Succeeded { psp_ref },
            Ok(PspResponse::Failed { code }) => ChargeOutcome::Failed { code },
            Ok(PspResponse::InProgress) => ChargeOutcome::Unknown,
            Err(e) => {
                tracing::warn!(error = %e, "could not parse psp response");
                ChargeOutcome::Unknown
            }
        }
    }

    /// Used by the reconciler to resolve attempts stuck at pending
    pub async fn lookup(&self, idempotency_key: &str) -> ChargeLookup {
        let result = self
            .http
            .get(format!("{}/charges/{}", self.base_url, idempotency_key))
            .send()
            .await;

        let response = match result {
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => return ChargeLookup::NotFound,
            Ok(r) if r.status().is_success() => r,
            Ok(_) | Err(_) => return ChargeLookup::Unreachable,
        };

        match response.json::<PspResponse>().await {
            Ok(PspResponse::InProgress) => ChargeLookup::InProgress,
            Ok(PspResponse::Succeeded { psp_ref }) => ChargeLookup::Succeeded { psp_ref },
            Ok(PspResponse::Failed { code }) => ChargeLookup::Failed { code },
            Err(_) => ChargeLookup::Unreachable,
        }
    }
}
