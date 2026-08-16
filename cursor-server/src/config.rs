use std::{env, net::SocketAddr, str::FromStr, time::Duration};

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
}

impl FromStr for ProviderKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "openai-chat" => Ok(Self::OpenAiChat),
            "openai-responses" => Ok(Self::OpenAiResponses),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(Error::Config(format!(
                "unsupported CURSOR_PROVIDER: {other}"
            ))),
        }
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub database_url: String,
    pub provider: ProviderConfig,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let listen_addr = env::var("CURSOR_LISTEN_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:3000".into())
            .parse()
            .map_err(|error| Error::Config(format!("invalid CURSOR_LISTEN_ADDR: {error}")))?;
        let kind = env::var("CURSOR_PROVIDER")
            .unwrap_or_else(|_| "openai-chat".into())
            .parse()?;
        let default_base = match kind {
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            _ => "https://api.openai.com/v1",
        };
        Ok(Self {
            listen_addr,
            database_url: env::var("CURSOR_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://cursor-server.db".into()),
            provider: ProviderConfig {
                kind,
                base_url: env::var("CURSOR_PROVIDER_BASE_URL")
                    .unwrap_or_else(|_| default_base.into())
                    .trim_end_matches('/')
                    .into(),
                api_key: env::var("CURSOR_PROVIDER_API_KEY").unwrap_or_default(),
                model: env::var("CURSOR_MODEL").unwrap_or_else(|_| "gpt-5".into()),
                request_timeout: Duration::from_secs(
                    env::var("CURSOR_PROVIDER_TIMEOUT_SECONDS")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(300),
                ),
            },
        })
    }
}
