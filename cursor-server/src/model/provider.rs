use std::{fmt, str::FromStr};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

impl ProviderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai-chat",
            Self::OpenAiResponses => "openai-responses",
            Self::Anthropic => "anthropic",
        }
    }
}

impl fmt::Display for ProviderType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderType {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "openai-chat" => Ok(Self::OpenAiChat),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(Error::Config(format!("unsupported provider type: {value}"))),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderEndpoint {
    pub provider_id: i64,
    pub name: String,
    pub provider_type: ProviderType,
    pub base_url: String,
    pub has_api_key: bool,
    pub custom_headers: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ProviderEndpointSecret {
    pub endpoint: ProviderEndpoint,
    pub api_key: String,
    pub custom_headers: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderEndpointInput {
    pub name: String,
    pub provider_type: ProviderType,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "empty_object")]
    pub custom_headers: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderModelInput {
    pub model_id: String,
    pub display_name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_enabled: bool,
    pub reasoning_effort: Option<String>,
    #[serde(default = "empty_object")]
    pub extra_params: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderModel {
    pub model_hash: String,
    pub provider_id: i64,
    pub model_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub sort_order: i64,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub reasoning_enabled: bool,
    pub reasoning_effort: Option<String>,
    pub extra_params: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub fn normalize_base_url(value: &str) -> Result<String> {
    let mut url = Url::parse(value.trim())
        .map_err(|error| Error::Config(format!("invalid provider base URL: {error}")))?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::Config(
            "provider base URL cannot contain query or fragment".into(),
        ));
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub fn model_hash(base_url: &str, provider_type: ProviderType, model_id: &str) -> Result<String> {
    let base_url = normalize_base_url(base_url)?;
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return Err(Error::Config("model id cannot be empty".into()));
    }
    let mut digest = Sha256::new();
    digest.update(base_url.as_bytes());
    digest.update([0]);
    digest.update(provider_type.as_str().as_bytes());
    digest.update([0]);
    digest.update(model_id.as_bytes());
    Ok(hex::encode(&digest.finalize()[..4]))
}

pub fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "x-api-key" | "api-key" | "cookie" | "set-cookie"
    )
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

fn enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_uses_normalized_url_type_and_model_only() {
        let first = model_hash(
            "HTTPS://Example.COM/v1/",
            ProviderType::OpenAiChat,
            "model-a",
        )
        .unwrap();
        let second = model_hash(
            "https://example.com/v1",
            ProviderType::OpenAiChat,
            "model-a",
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first, "f246010a");
        assert_ne!(
            first,
            model_hash("https://example.com/v1", ProviderType::Anthropic, "model-a").unwrap()
        );
    }
}
