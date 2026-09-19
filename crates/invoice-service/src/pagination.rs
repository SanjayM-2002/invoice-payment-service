use serde::{Deserialize, Serialize};

const DEFAULT_LIMIT: i64 = 25;
const MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct Pagination {
    pub limit: Option<i64>,
    pub starting_after: Option<String>,
}

impl Pagination {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
}


#[derive(Debug, Serialize)]
pub struct List<T> {
    pub data: Vec<T>,
    pub has_more: bool,
}

impl<T> List<T> {
    pub fn from_rows(mut rows: Vec<T>, limit: i64) -> Self {
        let has_more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        Self {
            data: rows,
            has_more,
        }
    }
}
