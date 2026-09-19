use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use invoice_service::{build_app, config::Config, psp::PspClient, seed, state::AppState, workers};


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "invoice_service=debug,info".into()),
        )
        .init();

    let config = Config::from_env()?;

    let db = PgPoolOptions::new()
        .max_connections(50)
        .connect(&config.database_url)
        .await?;
    tracing::info!("connected to postgres");

    sqlx::migrate!("./migrations").run(&db).await?;
    tracing::info!("migrations up to date");

    seed::run(&db, &config.webhook_receiver_base_url).await?;

    let port = config.port;
    let state = AppState {
        db,
        psp: PspClient::new(config.psp_base_url.clone()),
        config: Arc::new(config),
    };

    // Background workers.
    workers::reconciler::spawn(state.clone());
    workers::delivery::spawn(state.clone());

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("listening on http://0.0.0.0:{port}");

    axum::serve(listener, build_app(state)).await?;
    Ok(())
}
