use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderValue, Response, StatusCode},
    routing::post,
    Router,
};
use tower_http::decompression::RequestDecompressionLayer;

use crate::{
    cursor::{
        bidi_append, connect,
        proto::{agent::v1 as agent, aiserver::v1 as ai},
        run_sse,
    },
    run::RunRegistry,
    Result,
};

pub fn router(registry: RunRegistry) -> Router {
    Router::new()
        .route("/agent.v1.AgentService/RunSSE", post(run_sse_handler))
        .route(
            "/aiserver.v1.BidiService/BidiAppend",
            post(bidi_append_handler),
        )
        .layer(DefaultBodyLimit::disable())
        .layer(RequestDecompressionLayer::new())
        .with_state(registry)
}

async fn run_sse_handler(
    State(registry): State<RunRegistry>,
    body: Bytes,
) -> Result<Response<axum::body::Body>> {
    let request: agent::BidiRequestId = connect::decode_unary(&body)?;
    run_sse::stream(&registry, &request.request_id).await
}

async fn bidi_append_handler(
    State(registry): State<RunRegistry>,
    body: Bytes,
) -> Result<Response<axum::body::Body>> {
    let request: ai::BidiAppendRequest = connect::decode_unary(&body)?;
    bidi_append::append(&registry, request).await?;
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    Ok(response)
}
