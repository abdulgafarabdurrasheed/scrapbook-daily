mod ai;
mod app;
mod config;
mod models;
mod slack;
mod sources;
mod store;
mod web;

use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, atomic::AtomicUsize},
};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::{app::AppState, config::Config};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let db_path = PathBuf::from(&config.database_path);
    store::initialize(&db_path)?;
    let state = AppState {
        config: config.clone(),
        client: reqwest::Client::builder()
            .user_agent("scrapbook-daily/0.1.0")
            .build()?,
        db_path: Arc::new(db_path),
        ai_key_rotation: Arc::new(AtomicUsize::new(0)),
    };

    slack::verify_tokens(&state).await?;

    let app = Router::new()
        .route("/health", get(web::health))
        .route("/slack/commands", post(web::slash_command))
        .route("/slack/interactions", post(web::interaction))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "scrapbook-daily listening");
    axum::serve(listener, app).await?;
    Ok(())
}
