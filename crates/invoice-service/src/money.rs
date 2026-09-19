use crate::error::ApiError;

// Upper bound for invoice line items
pub const MAX_LINE_ITEMS: usize = 100;
pub const MAX_QUANTITY: i32 = 10_000;
pub const MAX_UNIT_AMOUNT_CENTS: i64 = 100_000_000; // $1,000,000 per unit

/// quantity x unit price, in cents.
pub fn line_amount(quantity: i32, unit_amount_cents: i64) -> Result<i64, ApiError> {
    i64::from(quantity)
        .checked_mul(unit_amount_cents)
        .ok_or_else(|| ApiError::invalid("line item amount is too large", None))
}

/// Sum of every line, computed server-side
pub fn invoice_total(items: &[(i32, i64)]) -> Result<i64, ApiError> {
    let mut total: i64 = 0;
    for &(quantity, unit_amount_cents) in items {
        let amount = line_amount(quantity, unit_amount_cents)?;
        total = total
            .checked_add(amount)
            .ok_or_else(|| ApiError::invalid("invoice total is too large", None))?;
    }

    if total <= 0 {
        return Err(ApiError::invalid(
            "invoice total must be greater than zero",
            Some("line_items"),
        ));
    }

    Ok(total)
}
