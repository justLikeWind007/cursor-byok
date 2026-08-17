use std::time::Duration;

use axum::{http::header, response::IntoResponse, routing::post, Router};
use cursor_server::{
    model::{
        ModelInvocation, ModelRequest, ModelSpec, PromptSpec, ProviderEndpointInput,
        ProviderModelInput, ProviderType,
    },
    provider::{ModelEvent, Provider, ProviderRouter},
    store::Store,
};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn records_one_summary_and_raw_payloads_for_one_provider_request() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let directory = tempfile::tempdir().unwrap();
    let store = Store::connect(&format!(
        "sqlite://{}",
        directory.path().join("observability.db").display()
    ))
    .await
    .unwrap();
    store.set_detailed_logging(true).await.unwrap();
    let endpoint = store
        .create_provider(&ProviderEndpointInput {
            name: "test".into(),
            provider_type: ProviderType::OpenAiChat,
            base_url: format!("http://{address}/v1"),
            api_key: Some("not-recorded".into()),
            custom_headers: serde_json::json!({"x-safe":"visible","authorization":"hidden"}),
        })
        .await
        .unwrap();
    let model = store
        .save_provider_model(
            endpoint.provider_id,
            &ProviderModelInput {
                model_id: "actual-model".into(),
                display_name: "Display Model".into(),
                enabled: true,
                sort_order: 0,
                context_window_tokens: None,
                max_output_tokens: None,
                reasoning_enabled: false,
                reasoning_effort: None,
                extra_params: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    let provider = ProviderRouter::new(store.clone(), Duration::from_secs(5));
    let events = provider
        .stream(
            ModelInvocation {
                call_id: "call-1".into(),
                run_id: "run-1".into(),
                conversation_id: "conversation-1".into(),
                provider_call_index: 0,
                request: ModelRequest {
                    prompt: PromptSpec {
                        instructions: "system".into(),
                        tools: Vec::new(),
                    },
                    model: ModelSpec::new(model.model_hash),
                    history: Vec::new(),
                },
            },
            CancellationToken::new(),
        )
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(Result::is_ok));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelEvent::Done(_)))));

    let call = store.llm_call("call-1").await.unwrap().unwrap();
    assert_eq!(call.status, "completed");
    assert_eq!(call.total_tokens, Some(12));
    assert!(call.ttfb_ms.is_some());
    assert!(call.ttft_ms.is_some());
    let request = store.llm_call_request("call-1").await.unwrap().unwrap();
    assert_eq!(request.body["model"], "actual-model");
    assert_eq!(request.headers["x-safe"], "visible");
    assert!(request.headers.get("authorization").is_none());
    assert!(!store.llm_call_chunks("call-1").await.unwrap().is_empty());
    server.abort();
}
