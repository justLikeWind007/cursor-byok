use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use crate::{model::LlmCallSummary, Result};

use super::{CallDetail, ControlService};

#[derive(Deserialize)]
pub struct CallQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

pub async fn list(
    State(service): State<ControlService>,
    Query(query): Query<CallQuery>,
) -> Result<Json<Vec<LlmCallSummary>>> {
    Ok(Json(service.calls(query.limit).await?))
}

pub async fn detail(
    State(service): State<ControlService>,
    Path(call_id): Path<String>,
) -> Result<Json<CallDetail>> {
    Ok(Json(service.call(&call_id).await?))
}

fn default_limit() -> i64 {
    100
}
