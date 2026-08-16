use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::{config::ProviderConfig, model::Usage, prompting::ModelRequest, Error, Result};

use super::{FinishReason, Provider, ProviderStream, ResponseEvent};

pub struct OpenAiResponsesProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

impl OpenAiResponsesProvider {
    pub fn new(client: reqwest::Client, config: ProviderConfig) -> Self {
        Self { client, config }
    }
}

impl Provider for OpenAiResponsesProvider {
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ProviderStream {
        let client = self.client.clone();
        let config = self.config.clone();
        Box::pin(try_stream! {
            let input = responses_input(&request.messages)?;
            let body = json!({
                "model": request.model, "input": input, "stream": true,
                "tools": request.tools.iter().map(|tool| json!({
                    "type":"function", "name":tool.name, "description":tool.description,
                    "parameters":tool.input_schema, "strict":false
                })).collect::<Vec<_>>()
            });
            let response = client.post(format!("{}/responses", config.base_url))
                .bearer_auth(&config.api_key).json(&body).send().await?;
            if !response.status().is_success() {
                let status = response.status(); let text = response.text().await?;
                Err(Error::Provider(format!("OpenAI Responses {status}: {text}")))?;
                return;
            }
            yield ResponseEvent::Start { model_call_id: request.model_call_id };
            let mut source = response.bytes_stream().eventsource();
            let mut text_open = false;
            let mut thinking_open = false;
            let mut tool_indices = std::collections::BTreeSet::new();
            let mut finish = FinishReason::Stop;
            loop {
                let event = tokio::select! {
                    _ = cancellation.cancelled() => { yield ResponseEvent::Done(FinishReason::Aborted); return; }
                    event = source.next() => event,
                };
                let Some(event) = event else { break };
                let event = event.map_err(|error| Error::Provider(format!("OpenAI Responses SSE: {error}")))?;
                if event.data == "[DONE]" { break; }
                let value: Value = serde_json::from_str(&event.data)?;
                let kind = value.get("type").and_then(Value::as_str).unwrap_or(&event.event);
                match kind {
                    "response.output_text.delta" => {
                        if thinking_open { thinking_open = false; yield ResponseEvent::ThinkingEnd; }
                        if !text_open { text_open = true; yield ResponseEvent::TextStart; }
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) { yield ResponseEvent::TextDelta(delta.into()); }
                    }
                    "response.reasoning_summary_text.delta" => {
                        if !thinking_open { thinking_open = true; yield ResponseEvent::ThinkingStart; }
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) { yield ResponseEvent::ThinkingDelta(delta.into()); }
                    }
                    "response.output_item.added" => {
                        let item = value.get("item").unwrap_or(&Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let index = required_u64(&value, "output_index")? as usize;
                            let call_id = required_string(item, "call_id")?.to_string();
                            let name = required_string(item, "name")?.to_string();
                            tool_indices.insert(index); finish = FinishReason::ToolUse;
                            yield ResponseEvent::ToolCallStart { index, call_id, name };
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let index = required_u64(&value, "output_index")? as usize;
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            yield ResponseEvent::ToolCallArgumentsDelta { index, delta: delta.into() };
                        }
                    }
                    "response.function_call_arguments.done" => {
                        let index = required_u64(&value, "output_index")? as usize;
                        if tool_indices.remove(&index) { yield ResponseEvent::ToolCallEnd { index }; }
                    }
                    "response.completed" => {
                        if let Some(usage) = value.pointer("/response/usage") { yield ResponseEvent::Usage(responses_usage(usage)); }
                    }
                    "response.incomplete" => finish = FinishReason::Length,
                    "response.failed" => finish = FinishReason::Error,
                    _ => {}
                }
            }
            if thinking_open { yield ResponseEvent::ThinkingEnd; }
            if text_open { yield ResponseEvent::TextEnd; }
            for index in tool_indices { yield ResponseEvent::ToolCallEnd { index }; }
            yield ResponseEvent::Done(finish);
        })
    }
}

fn responses_input(messages: &[crate::prompting::ProviderMessage]) -> Result<Vec<Value>> {
    let mut input = Vec::new();
    for message in messages {
        if message.role == "tool" {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.as_deref().ok_or_else(|| Error::Protocol("tool message is missing call_id".into()))?,
                "output": content_text(&message.content)?,
            }));
            continue;
        }
        let content_type = if message.role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let text = content_text(&message.content)?;
        if !text.is_empty() {
            input.push(json!({
                "type": "message", "role": message.role,
                "content": [{"type": content_type, "text": text}]
            }));
        }
        for call in message.tool_calls.iter().flatten() {
            input.push(json!({
                "type": "function_call",
                "call_id": required_string(call, "id")?,
                "name": call.pointer("/function/name").and_then(Value::as_str).ok_or_else(|| Error::Protocol("tool call is missing function.name".into()))?,
                "arguments": call.pointer("/function/arguments").and_then(Value::as_str).ok_or_else(|| Error::Protocol("tool call is missing function.arguments".into()))?,
            }));
        }
    }
    Ok(input)
}

fn content_text(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| Error::Protocol("provider message content must be a string".into()))
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event is missing {name}")))
}

fn required_u64(value: &Value, name: &str) -> Result<u64> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event is missing {name}")))
}

fn responses_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: value
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}
