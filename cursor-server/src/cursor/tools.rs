use std::collections::{BTreeMap, HashSet};

use crate::{
    model::{CanonicalMessage, MessageContent, Origin, Role, ToolCall},
    Error, Result,
};

use super::{
    exec, interaction,
    pending::{ExecContext, PendingClientTools, PendingExecRegistry},
    proto::agent::v1 as pb,
    tool_result::{self, ToolCompletion},
};

#[derive(Clone)]
pub struct ToolDispatcher {
    pending_execs: PendingExecRegistry,
    pending_interactions: PendingClientTools,
}

pub struct DispatchedTool {
    pub messages: Vec<pb::AgentServerMessage>,
    pub completion: Option<ToolCompletion>,
}

pub enum ClientToolEvent {
    Message(Box<pb::AgentServerMessage>),
    Completed(Box<ToolCompletion>),
}

impl ToolDispatcher {
    pub fn new(
        pending_execs: PendingExecRegistry,
        pending_interactions: PendingClientTools,
    ) -> Self {
        Self {
            pending_execs,
            pending_interactions,
        }
    }

    pub async fn start_batch(
        &self,
        calls: &[ToolCall],
        completed: &HashSet<String>,
        messages: &[CanonicalMessage],
        response_text: &str,
        response_thinking: &str,
        dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
        context: &ExecContext,
    ) -> Result<Vec<DispatchedTool>> {
        let first_tool_index = current_turn_step_count(messages)
            + usize::from(!response_thinking.is_empty())
            + usize::from(!response_text.is_empty())
            + 1;
        let mut dispatched = Vec::with_capacity(calls.len() - completed.len().min(calls.len()));
        for (position, call) in calls.iter().enumerate() {
            if completed.contains(&call.call_id) {
                continue;
            }
            dispatched.push(
                self.start(call, first_tool_index + position, dynamic_mcp, context)
                    .await?,
            );
        }
        Ok(dispatched)
    }

    async fn start(
        &self,
        call: &ToolCall,
        message_index: usize,
        dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
        context: &ExecContext,
    ) -> Result<DispatchedTool> {
        let mut messages = vec![interaction::tool_started(call)?];
        let completion = if let Some(definition) = dynamic_mcp.get(&call.name) {
            let id = self.pending_execs.reserve(call, context).await?;
            messages.push(exec::mcp_request(id, call, definition)?);
            None
        } else {
            match normalized(&call.name).as_str() {
                "shell"
                | "forcebackgroundshell"
                | "read"
                | "write"
                | "delete"
                | "grep"
                | "glob"
                | "ls"
                | "readlints"
                | "patchedit"
                | "writeshellstdin"
                | "task"
                | "callmcptool"
                | "fetchmcpresource" => {
                    let id = self.pending_execs.reserve(call, context).await?;
                    messages.push(exec::request(id, call, context)?);
                    None
                }
                "askquestion" | "websearch" | "webfetch" | "switchmode" | "createplan"
                | "generateimage" => {
                    let id = self.pending_interactions.reserve(call, context).await?;
                    messages.push(interaction::tool_query(id, call)?);
                    None
                }
                "todowrite" | "communicateupdate" => Some(tool_result::local(call, message_index)?),
                _ => return Err(Error::Protocol(format!("unsupported tool: {}", call.name))),
            }
        };
        Ok(DispatchedTool {
            messages,
            completion,
        })
    }

    pub async fn interaction_response(
        &self,
        response: &pb::InteractionResponse,
    ) -> Result<ClientToolEvent> {
        let pending = self
            .pending_interactions
            .take(response.id)
            .await
            .ok_or_else(|| {
                Error::Protocol(format!("unknown InteractionResponse id: {}", response.id))
            })?;
        if normalized(&pending.call.name) == "webfetch"
            && matches!(
                response.result.as_ref(),
                Some(pb::interaction_response::Result::WebFetchRequestResponse(
                    pb::WebFetchRequestResponse {
                        result: Some(pb::web_fetch_request_response::Result::Approved(_)),
                    }
                ))
            )
        {
            let id = self
                .pending_execs
                .reserve(&pending.call, &pending.context)
                .await?;
            return Ok(ClientToolEvent::Message(Box::new(exec::request(
                id,
                &pending.call,
                &pending.context,
            )?)));
        }
        Ok(ClientToolEvent::Completed(Box::new(
            tool_result::from_interaction(pending, response)?,
        )))
    }
}

fn current_turn_step_count(messages: &[CanonicalMessage]) -> usize {
    let turn_start = messages
        .iter()
        .rposition(|message| message.role == Role::User && message.origin == Origin::User)
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

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{CanonicalMessage, Origin, Role};

    fn call(name: &str) -> ToolCall {
        ToolCall {
            index: 0,
            call_id: "call-1".into(),
            model_call_id: "model-1".into(),
            name: name.into(),
            arguments_text: "{}".into(),
            arguments: json!({}),
        }
    }

    #[tokio::test]
    async fn communicate_update_completes_locally_at_the_cursor_step_index() {
        let dispatcher = ToolDispatcher::new(
            PendingExecRegistry::default(),
            PendingClientTools::default(),
        );
        let calls = [ToolCall {
            arguments: json!({"current_step": "Reading"}),
            ..call("CommunicateUpdate")
        }];
        let user = CanonicalMessage::text("user", Role::User, Origin::User, "go");
        let dispatched = dispatcher
            .start_batch(
                &calls,
                &HashSet::new(),
                &[user],
                "I will inspect it.",
                "Need to read.",
                &BTreeMap::new(),
                &ExecContext::default(),
            )
            .await
            .unwrap();
        let completion = dispatched[0].completion.as_ref().unwrap();
        let Some(pb::tool_call::Tool::CommunicateUpdateToolCall(tool)) =
            completion.tool_call().tool.as_ref()
        else {
            panic!("expected CommunicateUpdateToolCall")
        };
        let Some(pb::communicate_update_result::Result::Success(success)) = tool
            .result
            .as_ref()
            .and_then(|result| result.result.as_ref())
        else {
            panic!("expected communicate update success")
        };
        assert_eq!(success.message_index, 3);
    }

    #[tokio::test]
    async fn approved_web_fetch_moves_from_interaction_to_exec() {
        let dispatcher = ToolDispatcher::new(
            PendingExecRegistry::default(),
            PendingClientTools::default(),
        );
        let calls = [ToolCall {
            arguments: json!({"url": "https://example.com"}),
            ..call("WebFetch")
        }];
        let dispatched = dispatcher
            .start_batch(
                &calls,
                &HashSet::new(),
                &[],
                "",
                "",
                &BTreeMap::new(),
                &ExecContext::default(),
            )
            .await
            .unwrap();
        let Some(pb::agent_server_message::Message::InteractionQuery(query)) =
            dispatched[0].messages[1].message.as_ref()
        else {
            panic!("expected WebFetch InteractionQuery")
        };
        let event = dispatcher
            .interaction_response(&pb::InteractionResponse {
                id: query.id,
                result: Some(pb::interaction_response::Result::WebFetchRequestResponse(
                    pb::WebFetchRequestResponse {
                        result: Some(pb::web_fetch_request_response::Result::Approved(
                            pb::web_fetch_request_response::Approved {},
                        )),
                    },
                )),
            })
            .await
            .unwrap();
        let ClientToolEvent::Message(message) = event else {
            panic!("approval must open the Exec phase")
        };
        let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) = message.message
        else {
            panic!("expected FetchArgs")
        };
        assert!(matches!(
            exec.message,
            Some(pb::exec_server_message::Message::FetchArgs(_))
        ));
        let event = exec::client_event(
            &pb::ExecClientMessage {
                id: exec.id,
                message: Some(pb::exec_client_message::Message::FetchResult(
                    pb::FetchResult {
                        result: Some(pb::fetch_result::Result::Success(pb::FetchSuccess {
                            url: "https://example.com".into(),
                            content: "hello".into(),
                            status_code: 200,
                            content_type: "text/html".into(),
                        })),
                    },
                )),
                ..Default::default()
            },
            &dispatcher.pending_execs,
        )
        .await
        .unwrap();
        let exec::ClientExecEvent::Completed(completion) = event else {
            panic!("FetchResult must complete WebFetch")
        };
        assert_eq!(completion.result().output, json!("hello"));
        assert!(matches!(
            completion.tool_call().tool,
            Some(pb::tool_call::Tool::WebFetchToolCall(_))
        ));
    }
}
