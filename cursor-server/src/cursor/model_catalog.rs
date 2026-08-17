use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{Request, Response},
};
use bytes::{BufMut, BytesMut};
use prost::Message;

use crate::{
    cursor::{
        proto::agent::v1 as agent,
        proxy::{self, CursorProxy},
        CursorSessionRegistry,
    },
    model::ProviderModel,
    Error, Result,
};

#[derive(Clone, PartialEq, Message)]
struct AvailableModelsAddition {
    #[prost(string, repeated, tag = "1")]
    model_names: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    models: Vec<AvailableModel>,
}

#[derive(Clone, PartialEq, Message)]
struct AvailableModel {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(bool, optional, tag = "5")]
    supports_agent: Option<bool>,
    #[prost(bool, optional, tag = "9")]
    supports_thinking: Option<bool>,
    #[prost(int32, optional, tag = "15")]
    context_token_limit: Option<i32>,
    #[prost(string, optional, tag = "17")]
    client_display_name: Option<String>,
    #[prost(string, optional, tag = "18")]
    server_model_name: Option<String>,
    #[prost(bool, optional, tag = "23")]
    is_user_added: Option<bool>,
    #[prost(string, optional, tag = "24")]
    inputbox_short_model_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct UsableModelsAddition {
    #[prost(message, repeated, tag = "1")]
    models: Vec<agent::ModelDetails>,
}

pub async fn available_models(
    State(registry): State<CursorSessionRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let models = registry.store().provider_models(true).await?;
    merge_response(
        proxy::forward_buffered(&proxy, request).await?,
        AvailableModelsAddition {
            model_names: models
                .iter()
                .map(|model| model.model_hash.clone())
                .collect(),
            models: models.iter().map(available_model).collect(),
        }
        .encode_to_vec(),
    )
}

pub async fn usable_models(
    State(registry): State<CursorSessionRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let models = registry.store().provider_models(true).await?;
    merge_response(
        proxy::forward_buffered(&proxy, request).await?,
        UsableModelsAddition {
            models: models.iter().map(usable_model).collect(),
        }
        .encode_to_vec(),
    )
}

fn merge_response(upstream: proxy::BufferedResponse, extra: Vec<u8>) -> Result<Response<Body>> {
    if !upstream.status.is_success() {
        return Ok(upstream.into_response());
    }
    let (framed, payload) = unary_payload(&upstream.body)?;
    let body = if framed {
        let mut merged = BytesMut::with_capacity(5 + payload.len() + extra.len());
        merged.put_u8(0);
        merged.put_u32((payload.len() + extra.len()) as u32);
        merged.extend_from_slice(payload);
        merged.extend_from_slice(&extra);
        merged.freeze()
    } else {
        let mut merged = BytesMut::with_capacity(payload.len() + extra.len());
        merged.extend_from_slice(payload);
        merged.extend_from_slice(&extra);
        merged.freeze()
    };
    Ok(upstream.with_body(body))
}

fn unary_payload(body: &Bytes) -> Result<(bool, &[u8])> {
    if body.len() < 5 {
        return Ok((false, body));
    }
    let flags = body[0];
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    if length != body.len() - 5 {
        return Ok((false, body));
    }
    if flags != 0 {
        return Err(Error::Protocol(format!(
            "cannot merge compressed or terminal model catalog frame: flags={flags}"
        )));
    }
    Ok((true, &body[5..]))
}

fn available_model(model: &ProviderModel) -> AvailableModel {
    AvailableModel {
        name: model.model_hash.clone(),
        supports_agent: Some(true),
        supports_thinking: Some(model.reasoning_enabled),
        context_token_limit: model
            .context_window_tokens
            .map(|value| value.min(i32::MAX as u64) as i32),
        client_display_name: Some(model.display_name.clone()),
        server_model_name: Some(model.model_hash.clone()),
        is_user_added: Some(true),
        inputbox_short_model_name: Some(model.display_name.clone()),
    }
}

fn usable_model(model: &ProviderModel) -> agent::ModelDetails {
    agent::ModelDetails {
        model_id: model.model_hash.clone(),
        display_model_id: model.model_hash.clone(),
        display_name: model.display_name.clone(),
        display_name_short: model.display_name.clone(),
        thinking_details: model
            .reasoning_enabled
            .then_some(agent::ThinkingDetails::default()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{to_bytes, Bytes};

    use super::*;

    #[tokio::test]
    async fn appends_models_without_reencoding_official_fields() {
        // Unknown field 99 = 7 stands in for every official field this service does not know.
        let official = Bytes::from_static(&[0x98, 0x06, 0x07]);
        let addition = AvailableModelsAddition {
            model_names: vec!["f246010a".into()],
            models: Vec::new(),
        }
        .encode_to_vec();
        let response = merge_response(
            proxy::BufferedResponse {
                status: axum::http::StatusCode::OK,
                headers: Default::default(),
                body: official.clone(),
            },
            addition.clone(),
        )
        .unwrap();
        let merged = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&merged[..official.len()], official.as_ref());
        assert_eq!(&merged[official.len()..], addition);
    }

    #[tokio::test]
    async fn updates_connect_length_when_catalog_is_framed() {
        let official = [0x98, 0x06, 0x07];
        let mut framed = BytesMut::new();
        framed.put_u8(0);
        framed.put_u32(official.len() as u32);
        framed.extend_from_slice(&official);
        let response = merge_response(
            proxy::BufferedResponse {
                status: axum::http::StatusCode::OK,
                headers: Default::default(),
                body: framed.freeze(),
            },
            vec![0x0a, 0x01, b'x'],
        )
        .unwrap();
        let merged = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(u32::from_be_bytes(merged[1..5].try_into().unwrap()), 6);
        assert_eq!(&merged[5..8], &official);
    }
}
