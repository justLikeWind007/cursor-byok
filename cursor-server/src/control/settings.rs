use axum::{extract::State, Json};
use crate::Result;

use super::{ControlService, ObservabilitySettings};

pub async fn get(State(service): State<ControlService>) -> Result<Json<ObservabilitySettings>> {
    Ok(Json(service.observability().await?))
}

pub async fn update(
    State(service): State<ControlService>,
    Json(settings): Json<ObservabilitySettings>,
) -> Result<Json<ObservabilitySettings>> {
    Ok(Json(service.set_observability(settings).await?))
}
