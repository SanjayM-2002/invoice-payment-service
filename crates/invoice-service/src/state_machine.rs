//! The invoice state machine
//! 
//! Fresh invoice created -> open state
//! open invoice -> pay - successful -> paid state
//! open invoice -> mark void -> void state
//! open invoice -> write off -> uncollectible state -> can pay later -> paid state



use serde::Serialize;

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceStatus {
    Open,
    Paid,
    Void,
    Uncollectible,
}

impl InvoiceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Paid => "paid",
            Self::Void => "void",
            Self::Uncollectible => "uncollectible",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "open" => Self::Open,
            "paid" => Self::Paid,
            "void" => Self::Void,
            "uncollectible" => Self::Uncollectible,
            other => unreachable!("unknown invoice status in database: {other}"),
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Paid | Self::Void)
    }

    /// Open and Uncollectible state invoice can be paid
    pub fn is_payable(self) -> bool {
        matches!(self, Self::Open | Self::Uncollectible)
    }

    /// Validating invoice state transition
    pub fn can_transition_to(self, target: Self) -> bool {
        use InvoiceStatus::*;
        matches!(
            (self, target),
            (Open, Paid) | (Open, Void) | (Open, Uncollectible) | (Uncollectible, Paid)
        )
    }

    /// Rejected at the API layer, before any side effect, with a clear error.
    pub fn ensure_can_transition_to(self, target: Self) -> Result<(), ApiError> {
        if self.can_transition_to(target) {
            Ok(())
        } else {
            Err(ApiError::InvalidTransition {
                current_state: self.as_str().to_string(),
                attempted_state: target.as_str().to_string(),
            })
        }
    }
}
