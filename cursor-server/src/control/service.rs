use std::collections::BTreeSet;

use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::{
    model::{
        LlmCallRequest, LlmCallResponseChunk, LlmCallSummary, ProviderEndpoint,
        ProviderEndpointInput, ProviderEndpointSecret, ProviderModel, ProviderModelInput,
        ProviderType,
    },
    store::Store,
    Error, Result,
};

#[derive(Clone)]
pub struct ControlService {
    store: Store,
    client: reqwest::Client,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscoveredModels {
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CallDetail {
    pub call: LlmCallSummary,
    pub request: Option<LlmCallRequest>,
    pub response_chunks: Vec<LlmCallResponseChunk>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct ObservabilitySettings {
    pub detailed: bool,
}

impl ControlService {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            client: reqwest::Client::new(),
        }
    }

    pub async fn providers(&self) -> Result<Vec<ProviderEndpoint>> {
        self.store.providers().await
    }

    pub async fn create_provider(
        &self,
        input: &ProviderEndpointInput,
    ) -> Result<ProviderEndpoint> {
        self.store.create_provider(input).await
    }

    pub async fn update_provider(
        &self,
        provider_id: i64,
        input: &ProviderEndpointInput,
    ) -> Result<ProviderEndpoint> {
        self.store.update_provider(provider_id, input).await
    }

    pub async fn delete_provider(&self, provider_id: i64) -> Result<()> {
        self.store.delete_provider(provider_id).await
    }

    pub async fn models(&self) -> Result<Vec<ProviderModel>> {
        self.store.provider_models(false).await
    }

    pub async fn save_models(
        &self,
        provider_id: i64,
        models: &[ProviderModelInput],
    ) -> Result<Vec<ProviderModel>> {
        let mut saved = Vec::with_capacity(models.len());
        for model in models {
            saved.push(self.store.save_provider_model(provider_id, model).await?);
        }
        Ok(saved)
    }

    pub async fn delete_model(&self, model_hash: &str) -> Result<()> {
        self.store.delete_provider_model(model_hash).await
    }

    pub async fn discover_models(&self, provider_id: i64) -> Result<DiscoveredModels> {
        let provider = self
            .store
            .provider(provider_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("provider {provider_id}")))?;
        let mut models = match provider.endpoint.provider_type {
            ProviderType::OpenAiChat | ProviderType::OpenAiResponses => {
                openai_models(&self.client, &provider).await?
            }
            ProviderType::Anthropic => anthropic_models(&self.client, &provider).await?,
        };
        models.sort();
        models.dedup();
        Ok(DiscoveredModels { models })
    }

    pub async fn calls(&self, limit: i64) -> Result<Vec<LlmCallSummary>> {
        self.store.llm_calls(limit).await
    }

    pub async fn call(&self, call_id: &str) -> Result<CallDetail> {
        let call = self
            .store
            .llm_call(call_id)
            .await?
            .ok_or_else(|| Error::RunNotFound(format!("LLM call {call_id}")))?;
        Ok(CallDetail {
            request: self.store.llm_call_request(call_id).await?,
            response_chunks: self.store.llm_call_chunks(call_id).await?,
            call,
        })
    }

    pub async fn observability(&self) -> Result<ObservabilitySettings> {
        Ok(ObservabilitySettings {
            detailed: self.store.detailed_logging().await?,
        })
    }

    pub async fn set_observability(
        &self,
        settings: ObservabilitySettings,
    ) -> Result<ObservabilitySettings> {
        self.store.set_detailed_logging(settings.detailed).await?;
        Ok(settings)
    }
}

async fn openai_models(
    client: &reqwest::Client,
    provider: &ProviderEndpointSecret,
) -> Result<Vec<String>> {
    let mut request = client.get(format!("{}/models", provider.endpoint.base_url));
    if !provider.api_key.is_empty() {
        request = request.bearer_auth(&provider.api_key);
    }
    let response = apply_custom_headers(request, &provider.custom_headers)?
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    if !status.is_success() {
        return Err(Error::Provider(format!(
            "model discovery failed ({status}): {body}"
        )));
    }
    Ok(model_ids(body.get("data").unwrap_or(&body)))
}

async fn anthropic_models(
    client: &reqwest::Client,
    provider: &ProviderEndpointSecret,
) -> Result<Vec<String>> {
    let mut after_id = None::<String>;
    let mut found = BTreeSet::new();
    loop {
        let mut request = client
            .get(format!("{}/models", provider.endpoint.base_url))
            .query(&[("limit", "100")])
            .header("anthropic-version", "2023-06-01");
        if !provider.api_key.is_empty() {
            request = request.header("x-api-key", &provider.api_key);
        }
        if let Some(after_id) = &after_id {
            request = request.query(&[("after_id", after_id)]);
        }
        let response = apply_custom_headers(request, &provider.custom_headers)?
            .send()
            .await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await?;
        if !status.is_success() {
            return Err(Error::Provider(format!(
                "model discovery failed ({status}): {body}"
            )));
        }
        found.extend(model_ids(body.get("data").unwrap_or(&body)));
        if body.get("has_more").and_then(serde_json::Value::as_bool) != Some(true) {
            break;
        }
        after_id = body
            .get("last_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if after_id.is_none() {
            return Err(Error::Provider(
                "Anthropic model response has_more without last_id".into(),
            ));
        }
    }
    Ok(found.into_iter().collect())
}

fn model_ids(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            serde_json::Value::String(id) => Some(id.clone()),
            serde_json::Value::Object(object) => object
                .get("id")
                .or_else(|| object.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn apply_custom_headers(
    mut request: reqwest::RequestBuilder,
    headers: &serde_json::Value,
) -> Result<reqwest::RequestBuilder> {
    let object = headers
        .as_object()
        .ok_or_else(|| Error::Config("custom headers must be an object".into()))?;
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| Error::Config(format!("custom header {name} must be a string")))?;
        let name = HeaderName::try_from(name)
            .map_err(|error| Error::Config(format!("invalid header name: {error}")))?;
        let value = HeaderValue::try_from(value)
            .map_err(|error| Error::Config(format!("invalid header value: {error}")))?;
        request = request.header(name, value);
    }
    Ok(request)
}
