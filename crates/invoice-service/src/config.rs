use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub psp_base_url: String,
    pub webhook_receiver_base_url: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: required("DATABASE_URL")?,
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .context("PORT must be a number")?,
            psp_base_url: std::env::var("PSP_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:9100".to_string()),
            webhook_receiver_base_url: std::env::var("WEBHOOK_RECEIVER_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:9000".to_string()),
        })
    }
}

fn required(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var: {key}"))
}
