use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::{
    model::{ProviderEndpoint, ProviderEndpointInput},
    Result,
};

use super::ControlService;

pub async fn list(State(service): State<ControlService>) -> Result<Json<Vec<ProviderEndpoint>>> {
    Ok(Json(service.providers().await?))
}

pub async fn create(
    State(service): State<ControlService>,
    Json(input): Json<ProviderEndpointInput>,
) -> Result<(StatusCode, Json<ProviderEndpoint>)> {
    Ok((
        StatusCode::CREATED,
        Json(service.create_provider(&input).await?),
    ))
}

pub async fn update(
    State(service): State<ControlService>,
    Path(provider_id): Path<i64>,
    Json(input): Json<ProviderEndpointInput>,
) -> Result<Json<ProviderEndpoint>> {
    Ok(Json(
        service.update_provider(provider_id, &input).await?,
    ))
}

pub async fn remove(
    State(service): State<ControlService>,
    Path(provider_id): Path<i64>,
) -> Result<StatusCode> {
    service.delete_provider(provider_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
