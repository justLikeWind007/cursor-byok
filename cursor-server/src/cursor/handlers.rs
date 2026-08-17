use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    routing::post,
    Router,
};
use tower_http::decompression::RequestDecompressionLayer;

use crate::{
    cursor::{
        bidi_append, connect, model_catalog,
        proto::{agent::v1 as agent, aiserver::v1 as ai},
        proxy::{self, CursorProxy},
        run_sse,
    },
    cursor::{CursorParent, CursorSessionRegistry},
    Result,
};

pub fn router(registry: CursorSessionRegistry) -> Result<Router> {
    let proxy = CursorProxy::cursor()?;
    Ok(Router::new()
        .route("/agent.v1.AgentService/RunSSE", post(run_sse_handler))
        .route(
            "/aiserver.v1.BidiService/BidiAppend",
            post(bidi_append_handler),
        )
        .route(
            "/aiserver.v1.AiService/AvailableModels",
            post(model_catalog::available_models),
        )
        .route(
            "/agent.v1.AgentService/GetUsableModels",
            post(model_catalog::usable_models),
        )
        .route(
            "/aiserver.v1.AiService/GetUsableModels",
            post(model_catalog::usable_models),
        )
        .route_layer(DefaultBodyLimit::disable())
        .route_layer(RequestDecompressionLayer::new())
        .fallback(proxy::forward)
        .method_not_allowed_fallback(proxy::forward)
        .layer(Extension(proxy))
        .with_state(registry))
}

async fn run_sse_handler(
    State(registry): State<CursorSessionRegistry>,
    body: Bytes,
) -> Result<Response<axum::body::Body>> {
    let request: agent::BidiRequestId = connect::decode_unary(&body)?;
    run_sse::stream(&registry, &request.request_id).await
}

async fn bidi_append_handler(
    State(registry): State<CursorSessionRegistry>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<axum::body::Body>> {
    let request: ai::BidiAppendRequest = connect::decode_unary(&body)?;
    let parent = parent_headers(&headers)?;
    bidi_append::append(&registry, request, parent).await?;
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    Ok(response)
}

fn parent_headers(headers: &HeaderMap) -> Result<Option<CursorParent>> {
    let run_id = header_text(headers, "x-parent-request-id")?;
    let tool_call_id = header_text(headers, "x-parent-agent-tool-call-id")?;
    match (run_id, tool_call_id) {
        (None, None) => Ok(None),
        (Some(run_id), Some(tool_call_id)) => Ok(Some(CursorParent {
            run_id: run_id.into(),
            tool_call_id: tool_call_id.into(),
        })),
        _ => Err(crate::Error::Protocol(
            "Cursor subagent request must include both parent headers".into(),
        )),
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>> {
    headers
        .get(name)
        .map(|value| value.to_str())
        .transpose()
        .map_err(|error| crate::Error::Protocol(format!("invalid {name} header: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_parent_headers_are_an_atomic_pair() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-parent-request-id",
            HeaderValue::from_static("parent-run"),
        );
        assert!(parent_headers(&headers).is_err());

        headers.insert(
            "x-parent-agent-tool-call-id",
            HeaderValue::from_static("parent-call"),
        );
        assert_eq!(
            parent_headers(&headers).unwrap(),
            Some(CursorParent {
                run_id: "parent-run".into(),
                tool_call_id: "parent-call".into(),
            })
        );
    }
}
