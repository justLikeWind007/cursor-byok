use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::{config::ProviderConfig, model::Usage, prompting::ModelRequest, Error, Result};

use super::{FinishReason, Provider, ProviderStream, ResponseEvent};

pub struct AnthropicProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

impl AnthropicProvider {
    pub fn new(client: reqwest::Client, config: ProviderConfig) -> Self {
        Self { client, config }
    }
}

impl Provider for AnthropicProvider {
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ProviderStream {
        let client = self.client.clone();
        let config = self.config.clone();
        Box::pin(try_stream! {
            let mut system = String::new();
            let messages = anthropic_messages(&request.messages, &mut system)?;
            let body = json!({
                "model": request.model, "system": system, "messages": messages,
                "max_tokens": 32768, "stream": true,
                "tools": request.tools.iter().map(|tool| json!({
                    "name": tool.name, "description": tool.description, "input_schema": tool.input_schema
                })).collect::<Vec<_>>()
            });
            let response = client.post(format!("{}/messages", config.base_url))
                .header("x-api-key", &config.api_key).header("anthropic-version", "2023-06-01")
                .json(&body).send().await?;
            if !response.status().is_success() {
                let status = response.status(); let text = response.text().await?;
                Err(Error::Provider(format!("Anthropic {status}: {text}")))?;
                return;
            }
            yield ResponseEvent::Start { model_call_id: request.model_call_id };
            let mut source = response.bytes_stream().eventsource();
            let mut block_types = std::collections::HashMap::<usize, String>::new();
            let mut finish = FinishReason::Stop;
            while let Some(event) = tokio::select! {
                _ = cancellation.cancelled() => { yield ResponseEvent::Done(FinishReason::Aborted); return; }
                event = source.next() => event,
            } {
                let event = event.map_err(|error| Error::Provider(format!("Anthropic SSE: {error}")))?;
                let value: Value = serde_json::from_str(&event.data)?;
                match event.event.as_str() {
                    "message_start" => if let Some(usage) = value.pointer("/message/usage") { yield ResponseEvent::Usage(anthropic_usage(usage)); },
                    "content_block_start" => {
                        let index = required_u64(&value, "index")? as usize;
                        let block = value.get("content_block").unwrap_or(&Value::Null);
                        let kind = required_string(block, "type")?;
                        block_types.insert(index, kind.into());
                        match kind {
                            "text" => yield ResponseEvent::TextStart,
                            "thinking" => yield ResponseEvent::ThinkingStart,
                            "tool_use" => {
                                finish = FinishReason::ToolUse;
                                yield ResponseEvent::ToolCallStart {
                                    index,
                                    call_id: required_string(block, "id")?.into(),
                                    name: required_string(block, "name")?.into(),
                                };
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let index = required_u64(&value, "index")? as usize;
                        let delta = value.get("delta").unwrap_or(&Value::Null);
                        match required_string(delta, "type")? {
                            "text_delta" => if let Some(text) = delta.get("text").and_then(Value::as_str) { yield ResponseEvent::TextDelta(text.into()); },
                            "thinking_delta" => if let Some(text) = delta.get("thinking").and_then(Value::as_str) { yield ResponseEvent::ThinkingDelta(text.into()); },
                            "input_json_delta" => if let Some(text) = delta.get("partial_json").and_then(Value::as_str) { yield ResponseEvent::ToolCallArgumentsDelta { index, delta: text.into() }; },
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let index = required_u64(&value, "index")? as usize;
                        match block_types.remove(&index).as_deref() {
                            Some("text") => yield ResponseEvent::TextEnd,
                            Some("thinking") => yield ResponseEvent::ThinkingEnd,
                            Some("tool_use") => yield ResponseEvent::ToolCallEnd { index },
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(usage) = value.get("usage") { yield ResponseEvent::Usage(anthropic_usage(usage)); }
                        finish = match value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                            Some("tool_use") => FinishReason::ToolUse, Some("max_tokens") => FinishReason::Length,
                            Some("end_turn") | Some("stop_sequence") | None => finish,
                            Some(other) => Err(Error::Provider(format!("unknown Anthropic stop_reason: {other}")))?,
                        };
                    }
                    "error" => Err(Error::Provider(format!("Anthropic stream error: {}", event.data)))?,
                    _ => {}
                }
            }
            yield ResponseEvent::Done(finish);
        })
    }
}

fn anthropic_messages(
    messages: &[crate::prompting::ProviderMessage],
    system: &mut String,
) -> Result<Vec<Value>> {
    let mut output = Vec::new();
    for message in messages {
        if message.role == "system" {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(content_text(&message.content)?);
            continue;
        }
        if message.role == "tool" {
            push_anthropic(
                &mut output,
                "user",
                vec![json!({
                    "type":"tool_result", "tool_use_id":message.tool_call_id.as_deref().ok_or_else(|| Error::Protocol("tool message is missing call_id".into()))?,
                    "content":content_text(&message.content)?
                })],
            );
            continue;
        }
        let mut content = Vec::new();
        let text = content_text(&message.content)?;
        if !text.is_empty() {
            content.push(json!({"type":"text", "text":text}));
        }
        for call in message.tool_calls.iter().flatten() {
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Protocol("tool call is missing function.arguments".into()))?;
            content.push(json!({
                "type":"tool_use", "id":call.get("id").and_then(Value::as_str).ok_or_else(|| Error::Protocol("tool call is missing id".into()))?,
                "name":call.pointer("/function/name").and_then(Value::as_str).ok_or_else(|| Error::Protocol("tool call is missing function.name".into()))?,
                "input":serde_json::from_str::<Value>(arguments)?
            }));
        }
        if !content.is_empty() {
            push_anthropic(&mut output, &message.role, content);
        }
    }
    Ok(output)
}

fn push_anthropic(output: &mut Vec<Value>, role: &str, mut content: Vec<Value>) {
    if let Some(last) = output
        .last_mut()
        .filter(|last| last.get("role").and_then(Value::as_str) == Some(role))
    {
        if let Some(existing) = last.get_mut("content").and_then(Value::as_array_mut) {
            existing.append(&mut content);
            return;
        }
    }
    output.push(json!({"role":role, "content":content}));
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
        .ok_or_else(|| Error::Provider(format!("Anthropic event is missing {name}")))
}

fn required_u64(value: &Value, name: &str) -> Result<u64> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider(format!("Anthropic event is missing {name}")))
}

fn anthropic_usage(value: &Value) -> Usage {
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
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: 0,
    }
}
