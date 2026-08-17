use std::collections::{BTreeMap, HashSet};

pub mod codec;
mod dispatch;
pub(crate) mod edit;
pub(crate) mod result;
pub mod runtime;
pub(crate) mod stream;

use crate::{
    model::{CanonicalMessage, MessageContent, Role, ToolCall},
    Error, Result,
};

use self::result::{ToolCompletion, ToolResultSender};
use super::{interaction, proto::agent::v1 as pb};
use runtime::{CursorToolRuntime, ExecContext};

#[derive(Clone)]
pub struct ToolDispatcher {
    runtime: CursorToolRuntime,
    results: ToolResultSender,
}

pub struct DispatchedTool {
    pub messages: Vec<pb::AgentServerMessage>,
    pub completion: Option<ToolCompletion>,
}

pub struct ToolBatchState<'a> {
    pub completed: &'a HashSet<String>,
    pub started: &'a HashSet<String>,
    pub response_text: &'a str,
    pub response_thinking: &'a str,
}

pub enum ClientToolEvent {
    Message(Box<pb::AgentServerMessage>),
    Completed(Box<ToolCompletion>),
}

impl ToolDispatcher {
    pub fn new(runtime: CursorToolRuntime) -> Self {
        let (results, _) = result::tool_result_channel();
        Self::with_results(runtime, results)
    }

    pub fn with_results(runtime: CursorToolRuntime, results: ToolResultSender) -> Self {
        Self { runtime, results }
    }

    pub async fn start_batch(
        &self,
        calls: &[ToolCall],
        state: ToolBatchState<'_>,
        messages: &[CanonicalMessage],
        dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
        context: &ExecContext,
    ) -> Result<Vec<DispatchedTool>> {
        let first_tool_index = current_turn_step_count(messages)
            + usize::from(!state.response_thinking.is_empty())
            + usize::from(!state.response_text.is_empty())
            + 1;
        let mut dispatched = Vec::new();
        for (position, call) in calls.iter().enumerate() {
            if state.completed.contains(&call.call_id) {
                continue;
            }
            dispatched.push(
                self.start(
                    call,
                    first_tool_index + position,
                    !state.started.contains(&call.call_id),
                    dynamic_mcp,
                    context,
                )
                .await?,
            );
        }
        Ok(dispatched)
    }

    async fn start(
        &self,
        call: &ToolCall,
        message_index: usize,
        publish_started: bool,
        dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
        context: &ExecContext,
    ) -> Result<DispatchedTool> {
        let mut messages = if publish_started {
            vec![interaction::tool_started(call)?]
        } else {
            Vec::new()
        };
        let started = dispatch::start(
            &self.runtime,
            &self.results,
            call,
            message_index,
            dynamic_mcp,
            context,
        )
        .await?;
        messages.extend(started.messages);
        Ok(DispatchedTool {
            messages,
            completion: started.completion,
        })
    }

    pub async fn interaction_response(
        &self,
        response: &pb::InteractionResponse,
    ) -> Result<ClientToolEvent> {
        let pending = match self.runtime.take_interaction(response.id).await {
            Some(pending) => pending,
            None if self.runtime.completed_call(response.id).await.is_some() => {
                return Err(Error::Protocol(format!(
                    "duplicate terminal InteractionResponse id: {}",
                    response.id
                )));
            }
            None => {
                return Err(Error::Protocol(format!(
                    "unknown InteractionResponse id: {}",
                    response.id
                )));
            }
        };
        Ok(
            match dispatch::resume_interaction(&self.runtime, pending, response).await? {
                dispatch::InteractionContinuation::Message(message) => {
                    ClientToolEvent::Message(message)
                }
                dispatch::InteractionContinuation::Completed(completion) => {
                    ClientToolEvent::Completed(completion)
                }
            },
        )
    }
}

fn current_turn_step_count(messages: &[CanonicalMessage]) -> usize {
    let turn_start = messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .map_or(0, |position| position + 1);
    messages[turn_start..]
        .iter()
        .map(|message| match &message.content {
            MessageContent::Assistant {
                text,
                thinking,
                tool_calls,
                ..
            } => {
                usize::from(!thinking.is_empty()) + usize::from(!text.is_empty()) + tool_calls.len()
            }
            _ => 0,
        })
        .sum()
}
