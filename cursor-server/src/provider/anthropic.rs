use async_stream::try_stream;
use base64::{engine::general_purpose::STANDARD, Engine};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::{
    config::ProviderConfig,
    model::{
        ContentPart, ModelInvocation, ModelLatency, ProjectedContent, ProjectedMessage, Role, Usage,
    },
    Error, Result,
};

use super::{
    merge_extra_params, recorder::recorded_headers, CallRecorder, FinishReason, ModelEvent,
    Provider, ProviderStream,
};

pub struct AnthropicProvider {
    client: reqwest::Client,
    config: ProviderConfig,
    recorder: Option<CallRecorder>,
}

impl AnthropicProvider {
    pub fn new(client: reqwest::Client, config: ProviderConfig) -> Self {
        Self {
            client,
            config,
            recorder: None,
        }
    }

    pub fn with_recorder(mut self, recorder: Option<CallRecorder>) -> Self {
        self.recorder = recorder;
        self
    }
}

impl Provider for AnthropicProvider {
    fn stream(
        &self,
        invocation: ModelInvocation,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ProviderStream {
        let client = self.client.clone();
        let config = self.config.clone();
        let recorder = self.recorder.clone();
        Box::pin(try_stream! {
            let ModelInvocation { call_id, request, .. } = invocation;
            let messages = anthropic_messages(&request.history)?;
            let max_tokens = request.model.max_output_tokens.or(config.max_output_tokens)
                .ok_or_else(|| Error::Config("Anthropic requires CURSOR_PROVIDER_MAX_OUTPUT_TOKENS".into()))?;
            let mut body = json!({
                "model": request.model.model_id, "system": request.prompt.instructions, "messages": messages,
                "max_tokens": max_tokens, "stream": true,
                "tools": request.prompt.tools.iter().map(|tool| json!({
                    "name": tool.name, "description": tool.description, "input_schema": tool.parameters
                })).collect::<Vec<_>>()
            });
            apply_model(&mut body, &request.model)?;
            merge_extra_params(&mut body, &request.model.extra_params)?;
            if let Some(recorder) = &recorder {
                recorder.request(recorded_headers(&config, &[("content-type", "application/json"), ("anthropic-version", "2023-06-01")]), &body).await?;
            }
            let request = client.post(format!("{}/messages", config.base_url))
                .header("x-api-key", &config.api_key).header("anthropic-version", "2023-06-01")
                .headers(config.custom_headers.clone())
                .json(&body).send();
            let response = tokio::select! {
                _ = cancellation.cancelled() => return,
                response = request => response,
            };
            let response = response?;
            if let Some(recorder) = &recorder {
                recorder.response_headers(response.status().as_u16()).await?;
            }
            if !response.status().is_success() {
                let status = response.status(); let bytes = response.bytes().await?;
                if let Some(recorder) = &recorder { recorder.response_chunk(&bytes).await?; }
                let text = String::from_utf8_lossy(&bytes);
                Err(Error::Provider(format!("Anthropic {status}: {text}")))?;
                return;
            }
            yield ModelEvent::Start { model_call_id: call_id };
            let chunk_recorder = recorder.clone();
            let chunks = response.bytes_stream()
                .map(|chunk| chunk.map_err(Error::from))
                .then(move |chunk| {
                    let recorder = chunk_recorder.clone();
                    async move {
                        let chunk = chunk?;
                        if let Some(recorder) = recorder { recorder.response_chunk(&chunk).await?; }
                        Ok::<_, Error>(chunk)
                    }
                });
            let source = chunks.eventsource();
            futures_util::pin_mut!(source);
            let mut block_types = std::collections::HashMap::<usize, String>::new();
            let mut thinking_text = std::collections::HashMap::<usize, String>::new();
            let mut thinking_signatures = std::collections::HashMap::<usize, String>::new();
            let mut thinking_blocks = Vec::new();
            let mut finish = None;
            let mut terminal = false;
            let mut final_usage = None::<Usage>;
            while let Some(event) = tokio::select! {
                _ = cancellation.cancelled() => { return; }
                event = source.next() => event,
            } {
                let event = event.map_err(|error| Error::Provider(format!("Anthropic SSE: {error}")))?;
                let value: Value = serde_json::from_str(&event.data)?;
                match event.event.as_str() {
                    "message_start" => if let Some(usage) = value.pointer("/message/usage") {
                        merge_usage(final_usage.get_or_insert_default(), anthropic_usage(usage));
                    },
                    "content_block_start" => {
                        let index = required_u64(&value, "index")? as usize;
                        let block = value.get("content_block").unwrap_or(&Value::Null);
                        let kind = required_string(block, "type")?;
                        block_types.insert(index, kind.into());
                        match kind {
                            "text" => yield ModelEvent::TextStart,
                            "thinking" => {
                                thinking_text.insert(index, String::new());
                                thinking_signatures.insert(index, String::new());
                                yield ModelEvent::ThinkingStart;
                            }
                            "tool_use" => {
                                yield ModelEvent::ToolCallStart {
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
                            "text_delta" => if let Some(text) = delta.get("text").and_then(Value::as_str) { yield ModelEvent::TextDelta(text.into()); },
                            "thinking_delta" => if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                                thinking_text.entry(index).or_default().push_str(text);
                                yield ModelEvent::ThinkingDelta(text.into());
                            },
                            "signature_delta" => if let Some(signature) = delta.get("signature").and_then(Value::as_str) {
                                thinking_signatures.entry(index).or_default().push_str(signature);
                            },
                            "input_json_delta" => if let Some(text) = delta.get("partial_json").and_then(Value::as_str) { yield ModelEvent::ToolCallArgumentsDelta { index, delta: text.into() }; },
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let index = required_u64(&value, "index")? as usize;
                        match block_types.remove(&index).as_deref() {
                            Some("text") => yield ModelEvent::TextEnd,
                            Some("thinking") => {
                                let thinking = thinking_text.remove(&index).unwrap_or_default();
                                let signature = thinking_signatures.remove(&index).unwrap_or_default();
                                if signature.is_empty() {
                                    Err(Error::Provider("Anthropic thinking block ended without signature".into()))?;
                                }
                                thinking_blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": thinking,
                                    "signature": signature,
                                }));
                                yield ModelEvent::ThinkingEnd;
                            }
                            Some("tool_use") => yield ModelEvent::ToolCallEnd { index },
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(usage) = value.get("usage") {
                            merge_usage(final_usage.get_or_insert_default(), anthropic_usage(usage));
                        }
                        finish = match value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                            Some("tool_use") => Some(FinishReason::ToolUse),
                            Some("max_tokens" | "model_context_window_exceeded") => Some(FinishReason::Length),
                            Some("end_turn") | Some("stop_sequence") => Some(FinishReason::Stop),
                            None => finish,
                            Some(other) => Err(Error::Provider(format!("unknown Anthropic stop_reason: {other}")))?,
                        };
                    }
                    "message_stop" => {
                        if !block_types.is_empty() {
                            Err(Error::Provider("Anthropic message_stop arrived with an open content block".into()))?;
                        }
                        terminal = true;
                        if !thinking_blocks.is_empty() {
                            yield ModelEvent::ProviderReplayState(
                                crate::model::ProviderReplayState {
                                    provider_kind: "anthropic".into(),
                                    value: json!({"blocks": std::mem::take(&mut thinking_blocks)}),
                                },
                            );
                        }
                        if let Some(usage) = final_usage {
                            yield ModelEvent::Usage(usage);
                        }
                        yield ModelEvent::Done(finish.ok_or_else(|| Error::Provider("Anthropic message_stop is missing stop_reason".into()))?);
                    }
                    "error" => Err(Error::Provider(format!("Anthropic stream error: {}", event.data)))?,
                    _ => {}
                }
            }
            if !terminal {
                Err(Error::Provider("Anthropic stream ended without message_stop".into()))?;
            }
        })
    }
}

fn apply_model(body: &mut Value, model: &crate::model::ModelSpec) -> Result<()> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| Error::Provider("Anthropic request body is not an object".into()))?;
    if model.reasoning.enabled {
        object.insert(
            "thinking".into(),
            json!({"type":"adaptive", "display":"summarized"}),
        );
    }
    if let Some(effort) = &model.reasoning.effort {
        object.insert("output_config".into(), json!({"effort":effort}));
    }
    if model.latency == ModelLatency::Fast {
        return Err(Error::Provider(
            "Anthropic route cannot satisfy fast model latency".into(),
        ));
    }
    Ok(())
}

fn merge_usage(total: &mut Usage, update: Usage) {
    merge_usage_field(&mut total.input_tokens, update.input_tokens);
    merge_usage_field(&mut total.output_tokens, update.output_tokens);
    merge_usage_field(&mut total.cache_read_tokens, update.cache_read_tokens);
    merge_usage_field(&mut total.cache_write_tokens, update.cache_write_tokens);
    merge_usage_field(&mut total.reasoning_tokens, update.reasoning_tokens);
}

fn merge_usage_field(total: &mut Option<u64>, update: Option<u64>) {
    if let Some(update) = update {
        *total = Some(total.map_or(update, |current| current.max(update)));
    }
}

fn anthropic_messages(messages: &[ProjectedMessage]) -> Result<Vec<Value>> {
    let mut output = Vec::new();
    for message in messages {
        match &message.content {
            ProjectedContent::Parts(parts) => {
                let content = anthropic_parts(&message.role, parts)?;
                if !content.is_empty() {
                    push_anthropic(&mut output, role_name(&message.role), content);
                }
            }
            ProjectedContent::ToolResult(result) => push_anthropic(
                &mut output,
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": result.call_id,
                    "content": result.content,
                })],
            ),
            ProjectedContent::Assistant {
                text,
                replay_state,
                calls,
                ..
            } => {
                let mut content = Vec::new();
                if let Some(blocks) = replay_state
                    .as_ref()
                    .filter(|state| state.provider_kind == "anthropic")
                    .and_then(|state| state.value.get("blocks"))
                    .and_then(Value::as_array)
                {
                    content.extend(blocks.iter().cloned());
                }
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                content.extend(calls.iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.call_id,
                        "name": call.name,
                        "input": call.arguments,
                    })
                }));
                if !content.is_empty() {
                    push_anthropic(&mut output, "assistant", content);
                }
            }
        }
    }
    Ok(output)
}

fn anthropic_parts(role: &Role, parts: &[ContentPart]) -> Result<Vec<Value>> {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } if text.is_empty() => None,
            ContentPart::Text { text } => Some(Ok(json!({"type":"text", "text":text}))),
            ContentPart::Image { mime_type, data } if *role == Role::User => Some(Ok(json!({
                "type":"image",
                "source":{
                    "type":"base64",
                    "media_type":mime_type,
                    "data":STANDARD.encode(data),
                },
            }))),
            ContentPart::Image { .. } => Some(Err(Error::Protocol(
                "Anthropic only accepts images in user messages".into(),
            ))),
        })
        .collect()
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

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::System => "user",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user",
    }
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
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
        total_tokens: value.get("total_tokens").and_then(Value::as_u64),
        cache_read_tokens: value.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_write_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
        reasoning_tokens: None,
    }
}
