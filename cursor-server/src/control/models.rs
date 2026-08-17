use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use crate::{
    model::{ProviderModel, ProviderModelInput},
    Result,
};

use super::{ControlService, DiscoveredModels};

#[derive(Deserialize)]
pub struct SaveModels {
    pub models: Vec<ProviderModelInput>,
}

pub async fn list(State(service): State<ControlService>) -> Result<Json<Vec<ProviderModel>>> {
    Ok(Json(service.models().await?))
}

pub async fn save(
    State(service): State<ControlService>,
    Path(provider_id): Path<i64>,
    Json(input): Json<SaveModels>,
) -> Result<(StatusCode, Json<Vec<ProviderModel>>)> {
    Ok((
        StatusCode::CREATED,
        Json(service.save_models(provider_id, &input.models).await?),
    ))
}

pub async fn remove(
    State(service): State<ControlService>,
    Path(model_hash): Path<String>,
) -> Result<StatusCode> {
    service.delete_model(&model_hash).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn discover(
    State(service): State<ControlService>,
    Path(provider_id): Path<i64>,
) -> Result<Json<DiscoveredModels>> {
    Ok(Json(service.discover_models(provider_id).await?))
}
