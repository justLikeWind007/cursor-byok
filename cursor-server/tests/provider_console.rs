use cursor_server::{
    model::{NewLlmCall, ProviderEndpointInput, ProviderModelInput, ProviderType, Usage},
    store::Store,
};

async fn store() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", directory.path().join("test.db").display());
    let store = Store::connect(&url).await.unwrap();
    (directory, store)
}

#[tokio::test]
async fn provider_secret_is_write_only_and_model_hash_is_stable() {
    let (_directory, store) = store().await;
    let provider = store
        .create_provider(&ProviderEndpointInput {
            name: "Local".into(),
            provider_type: ProviderType::OpenAiChat,
            base_url: "https://example.com/v1/".into(),
            api_key: Some("secret".into()),
            custom_headers: serde_json::json!({"x-route":"one", "authorization":"header-secret"}),
        })
        .await
        .unwrap();
    assert!(provider.has_api_key);
    assert!(!serde_json::to_string(&provider).unwrap().contains("secret"));
    assert_eq!(
        provider.custom_headers["authorization"],
        serde_json::Value::Null
    );
    let updated = store
        .update_provider(
            provider.provider_id,
            &ProviderEndpointInput {
                name: "Renamed".into(),
                provider_type: provider.provider_type,
                base_url: provider.base_url.clone(),
                api_key: None,
                custom_headers: provider.custom_headers.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(
        store
            .provider(provider.provider_id)
            .await
            .unwrap()
            .unwrap()
            .custom_headers["authorization"],
        "header-secret"
    );

    let model = store
        .save_provider_model(
            provider.provider_id,
            &ProviderModelInput {
                model_id: "model-a".into(),
                display_name: "Model A".into(),
                enabled: true,
                sort_order: 0,
                context_window_tokens: Some(128_000),
                max_output_tokens: Some(8_192),
                reasoning_enabled: false,
                reasoning_effort: None,
                extra_params: serde_json::json!({"temperature":0}),
            },
        )
        .await
        .unwrap();
    assert_eq!(model.model_hash, "f246010a");
}

#[tokio::test]
async fn call_summary_is_always_stored_and_payloads_follow_detailed_setting() {
    let (_directory, store) = store().await;
    let provider = store
        .create_provider(&ProviderEndpointInput {
            name: "Local".into(),
            provider_type: ProviderType::OpenAiChat,
            base_url: "https://example.com/v1".into(),
            api_key: None,
            custom_headers: serde_json::json!({}),
        })
        .await
        .unwrap();
    let model = store
        .save_provider_model(
            provider.provider_id,
            &ProviderModelInput {
                model_id: "model-a".into(),
                display_name: "Model A".into(),
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
    let call = NewLlmCall {
        call_id: "call-1".into(),
        run_id: "run-1".into(),
        conversation_id: "conversation-1".into(),
        provider_call_index: 0,
        model_hash: model.model_hash,
        provider_type: ProviderType::OpenAiChat,
        provider_url: provider.base_url,
        model_id: model.model_id,
        display_name: model.display_name,
        message_count: 2,
        tool_count: 3,
        detailed: false,
    };
    store.start_llm_call(&call).await.unwrap();
    store
        .record_llm_request(
            "call-1",
            &serde_json::json!({}),
            &serde_json::json!({"model":"model-a"}),
            false,
        )
        .await
        .unwrap();
    store
        .record_llm_chunk("call-1", 0, 4, b"data", false)
        .await
        .unwrap();
    store
        .record_llm_usage(
            "call-1",
            Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    store
        .finish_llm_call("call-1", "completed", Some("stop"), 9, None, None)
        .await
        .unwrap();

    let summary = store.llm_call("call-1").await.unwrap().unwrap();
    assert_eq!(summary.total_tokens, Some(15));
    assert_eq!(summary.request_bytes, Some(19));
    assert_eq!(summary.response_bytes, 4);
    assert!(store.llm_call_request("call-1").await.unwrap().is_none());
    assert!(store.llm_call_chunks("call-1").await.unwrap().is_empty());
}
