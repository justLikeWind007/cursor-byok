mod calls;
mod models;
mod providers;
mod service;
mod settings;

use axum::{
    routing::{delete, get, post, put},
    Router,
};
use tower_http::services::ServeDir;

pub use service::{CallDetail, ControlService, DiscoveredModels, ObservabilitySettings};

pub fn router(service: ControlService, assets: impl AsRef<std::path::Path>) -> Router {
    Router::new()
        .nest_service(
            "/console",
            ServeDir::new(assets).append_index_html_on_directories(true),
        )
        .route(
            "/api/providers",
            get(providers::list).post(providers::create),
        )
        .route(
            "/api/providers/{provider_id}",
            put(providers::update).delete(providers::remove),
        )
        .route(
            "/api/providers/{provider_id}/models/discover",
            post(models::discover),
        )
        .route("/api/providers/{provider_id}/models", post(models::save))
        .route("/api/models", get(models::list))
        .route("/api/models/{model_hash}", delete(models::remove))
        .route("/api/llm-calls", get(calls::list))
        .route("/api/llm-calls/{call_id}", get(calls::detail))
        .route(
            "/api/settings/observability",
            get(settings::get).put(settings::update),
        )
        .with_state(service)
}
