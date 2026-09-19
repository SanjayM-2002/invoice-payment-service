use uuid::Uuid;

/// Prefixed id, for eg: `cus_0199a0b57a2c77f491e63687d0d96c1d`
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7().simple())
}
