use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode},
};
use bytes::Bytes;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::{cursor::CursorSessionRegistry, Result};

pub async fn stream(registry: &CursorSessionRegistry, request_id: &str) -> Result<Response<Body>> {
    let receiver = registry.get_or_create(request_id).await?.subscribe();
    let body_stream =
        UnboundedReceiverStream::new(receiver).map(Ok::<Bytes, std::convert::Infallible>);
    let mut response = Response::new(Body::from_stream(body_stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("connect-protocol-version", HeaderValue::from_static("1"));
    Ok(response)
}
