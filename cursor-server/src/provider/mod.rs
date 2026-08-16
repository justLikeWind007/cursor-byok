mod anthropic;
mod event;
mod openai_chat;
mod openai_responses;

use std::{pin::Pin, sync::Arc};

use futures_util::Stream;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{ProviderConfig, ProviderKind},
    prompting::ModelRequest,
    Result,
};

pub use anthropic::AnthropicProvider;
pub use event::*;
pub use openai_chat::OpenAiChatProvider;
pub use openai_responses::OpenAiResponsesProvider;

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ResponseEvent>> + Send>>;

pub trait Provider: Send + Sync {
    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ProviderStream;
}

pub fn build_provider(config: &ProviderConfig) -> Result<Arc<dyn Provider>> {
    let client = reqwest::Client::builder()
        .timeout(config.request_timeout)
        .build()?;
    Ok(match config.kind {
        ProviderKind::OpenAiChat => Arc::new(OpenAiChatProvider::new(client, config.clone())),
        ProviderKind::OpenAiResponses => {
            Arc::new(OpenAiResponsesProvider::new(client, config.clone()))
        }
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(client, config.clone())),
    })
}
