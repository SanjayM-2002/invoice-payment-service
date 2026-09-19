use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, psp::PspClient};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub psp: PspClient,
    pub config: Arc<Config>,
}
