use std::{collections::btree_map::Entry, collections::BTreeMap, time::Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    client::ClientEvent,
    model::{ProviderReplayState, ToolCall, Usage},
    provider::{FinishReason, ModelEvent, ProviderStream},
};

use super::RunFailure;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCycleResult {
    pub model_call_id: String,
    pub text: String,
    pub reasoning: String,
    pub replay_state: Option<ProviderReplayState>,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    pub finish_reason: FinishReason,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelCycleFailure {
    pub failure: RunFailure,
    pub partial_text: String,
    pub partial_reasoning: String,
    pub usage: Option<Usage>,
}

struct OpenTool {
    call: ToolCall,
    ended: bool,
}

pub async fn consume_model_cycle(
    mut stream: ProviderStream,
    client: &mpsc::Sender<ClientEvent>,
    cancellation: &CancellationToken,
) -> std::result::Result<ModelCycleResult, ModelCycleFailure> {
    let mut model_call_id = None;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut text_open = false;
    let mut thinking_started = None::<Instant>;
    let mut tools = BTreeMap::<usize, OpenTool>::new();
    let mut call_ids = std::collections::HashSet::new();
    let mut replay_state = None;
    let mut usage = None;
    let mut finish = None;

    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(failure(RunFailure::Client("run was cancelled".into()), text, reasoning, usage));
            }
            next = stream.next() => next,
        };
        let Some(next) = next else { break };
        let event = match next {
            Ok(event) => event,
            Err(error) => {
                return Err(failure(error.into(), text, reasoning, usage));
            }
        };
        if finish.is_some() {
            return Err(failure(
                RunFailure::Protocol("provider emitted an event after Done".into()),
                text,
                reasoning,
                usage,
            ));
        }
        let result = match event {
            ModelEvent::Start { model_call_id: id } => {
                if model_call_id.replace(id).is_some() {
                    Err("provider emitted duplicate Start")
                } else {
                    Ok(())
                }
            }
            ModelEvent::TextStart => {
                if model_call_id.is_none() {
                    Err("provider emitted content before Start")
                } else if text_open {
                    Err("provider emitted duplicate TextStart")
                } else {
                    text_open = true;
                    send(client, ClientEvent::TextStart).await
                }
            }
            ModelEvent::TextDelta(delta) => {
                if !text_open {
                    Err("provider emitted TextDelta before TextStart")
                } else {
                    text.push_str(&delta);
                    send(client, ClientEvent::TextDelta(delta)).await
                }
            }
            ModelEvent::TextEnd => {
                if !text_open {
                    Err("provider emitted TextEnd before TextStart")
                } else {
                    text_open = false;
                    send(client, ClientEvent::TextEnd).await
                }
            }
            ModelEvent::ThinkingStart => {
                if model_call_id.is_none() {
                    Err("provider emitted content before Start")
                } else if thinking_started.replace(Instant::now()).is_some() {
                    Err("provider emitted duplicate ThinkingStart")
                } else {
                    send(client, ClientEvent::ThinkingStart).await
                }
            }
            ModelEvent::ThinkingDelta(delta) => {
                if thinking_started.is_none() {
                    Err("provider emitted ThinkingDelta before ThinkingStart")
                } else {
                    reasoning.push_str(&delta);
                    send(client, ClientEvent::ThinkingDelta(delta)).await
                }
            }
            ModelEvent::ThinkingEnd => {
                if let Some(started) = thinking_started.take() {
                    send(
                        client,
                        ClientEvent::ThinkingEnd {
                            duration: started.elapsed(),
                        },
                    )
                    .await
                } else {
                    Err("provider emitted ThinkingEnd before ThinkingStart")
                }
            }
            ModelEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                let Some(model_call_id) = model_call_id.as_ref() else {
                    return Err(failure(
                        RunFailure::Protocol("provider emitted content before Start".into()),
                        text,
                        reasoning,
                        usage,
                    ));
                };
                match tools.entry(index) {
                    Entry::Occupied(_) => Err("provider reused a tool index"),
                    Entry::Vacant(_) if !call_ids.insert(call_id.clone()) => {
                        Err("provider reused a tool call_id")
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(OpenTool {
                            call: ToolCall {
                                index,
                                call_id: call_id.clone(),
                                model_call_id: model_call_id.clone(),
                                name: name.clone(),
                                arguments_text: String::new(),
                                arguments: serde_json::Value::Null,
                            },
                            ended: false,
                        });
                        send(
                            client,
                            ClientEvent::ToolCallStart {
                                index,
                                call_id,
                                name,
                                model_call_id: model_call_id.clone(),
                            },
                        )
                        .await
                    }
                }
            }
            ModelEvent::ToolCallArgumentsDelta { index, delta } => match tools.get_mut(&index) {
                Some(tool) if !tool.ended => {
                    tool.call.arguments_text.push_str(&delta);
                    send(client, ClientEvent::ToolCallArgumentsDelta { index, delta }).await
                }
                Some(_) => Err("provider emitted tool arguments after ToolCallEnd"),
                None => Err("provider emitted tool arguments for an unknown index"),
            },
            ModelEvent::ToolCallEnd { index } => match tools.get_mut(&index) {
                Some(tool) if !tool.ended => {
                    match serde_json::from_str(&tool.call.arguments_text) {
                        Ok(arguments) => {
                            tool.call.arguments = arguments;
                            tool.ended = true;
                            send(client, ClientEvent::ToolCallEnd { index }).await
                        }
                        Err(_) => Err("provider ended a tool call with invalid JSON arguments"),
                    }
                }
                Some(_) => Err("provider emitted duplicate ToolCallEnd"),
                None => Err("provider ended an unknown tool index"),
            },
            ModelEvent::ProviderReplayState(state) => {
                if replay_state.replace(state).is_some() {
                    Err("provider emitted duplicate ProviderReplayState")
                } else {
                    Ok(())
                }
            }
            ModelEvent::Usage(value) => {
                if usage.replace(value).is_some() {
                    Err("provider emitted duplicate Usage")
                } else {
                    Ok(())
                }
            }
            ModelEvent::Done(reason) => {
                if model_call_id.is_none() {
                    Err("provider emitted content before Start")
                } else if text_open
                    || thinking_started.is_some()
                    || tools.values().any(|tool| !tool.ended)
                {
                    Err("provider emitted Done with an open content block")
                } else {
                    finish = Some(reason);
                    Ok(())
                }
            }
        };
        if let Err(message) = result {
            return Err(failure(
                RunFailure::Protocol(message.into()),
                text,
                reasoning,
                usage,
            ));
        }
    }

    let Some(finish_reason) = finish else {
        return Err(failure(
            RunFailure::Provider("provider stream reached EOF before Done".into()),
            text,
            reasoning,
            usage,
        ));
    };
    let calls = tools
        .into_values()
        .map(|tool| tool.call)
        .collect::<Vec<_>>();
    if finish_reason == FinishReason::Length {
        return Err(failure(
            RunFailure::Provider("model stopped before completing the response".into()),
            text,
            reasoning,
            usage,
        ));
    }
    if matches!(finish_reason, FinishReason::ToolUse) != !calls.is_empty() {
        return Err(failure(
            RunFailure::Protocol("finish reason and tool calls disagree".into()),
            text,
            reasoning,
            usage,
        ));
    }
    if let Some(usage) = usage {
        if send(client, ClientEvent::Usage(usage)).await.is_err() {
            return Err(failure(
                RunFailure::Client("client event channel closed".into()),
                text,
                reasoning,
                Some(usage),
            ));
        }
    }
    let model_call_id = model_call_id.ok_or_else(|| {
        failure(
            RunFailure::Protocol("provider completed without Start".into()),
            text.clone(),
            reasoning.clone(),
            usage,
        )
    })?;
    Ok(ModelCycleResult {
        model_call_id,
        text,
        reasoning,
        replay_state,
        calls,
        usage,
        finish_reason,
    })
}

async fn send(
    client: &mpsc::Sender<ClientEvent>,
    event: ClientEvent,
) -> std::result::Result<(), &'static str> {
    client
        .send(event)
        .await
        .map_err(|_| "client event channel closed")
}

fn failure(
    failure: RunFailure,
    partial_text: String,
    partial_reasoning: String,
    usage: Option<Usage>,
) -> ModelCycleFailure {
    ModelCycleFailure {
        failure,
        partial_text,
        partial_reasoning,
        usage,
    }
}
