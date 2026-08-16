use crate::{
    cursor::{
        proto::agent::v1 as pb,
        tool_result::{self, ToolCompletion},
    },
    model::{ToolCall, Usage},
    provider::ResponseEvent,
};
use serde_json::Value;
use std::time::Duration;

use crate::{Error, Result};

pub fn response_event(
    event: &ResponseEvent,
    model_call_id: &str,
) -> Result<Option<pb::AgentServerMessage>> {
    use pb::interaction_update::Message;
    let message = match event {
        ResponseEvent::TextDelta(text) => Message::TextDelta(pb::TextDeltaUpdate {
            text: text.clone(),
            is_server_notice: false,
        }),
        ResponseEvent::ThinkingDelta(text) => Message::ThinkingDelta(pb::ThinkingDeltaUpdate {
            text: text.clone(),
            thinking_style: Some(pb::ThinkingStyle::Default as i32),
        }),
        ResponseEvent::ToolCallStart { call_id, name, .. } => {
            Message::PartialToolCall(pb::PartialToolCallUpdate {
                call_id: call_id.clone(),
                tool_call: Some(tool_placeholder(name, call_id)?),
                args_text_delta: String::new(),
                model_call_id: model_call_id.into(),
            })
        }
        ResponseEvent::ToolCallArgumentsDelta { .. } => return Ok(None),
        ResponseEvent::ToolCallEnd { .. }
        | ResponseEvent::Start { .. }
        | ResponseEvent::TextStart
        | ResponseEvent::TextEnd
        | ResponseEvent::ThinkingStart
        | ResponseEvent::ThinkingEnd
        | ResponseEvent::Usage(_)
        | ResponseEvent::Done(_) => return Ok(None),
    };
    Ok(Some(server_interaction(message)))
}

pub fn thinking_completed(elapsed: Duration) -> pb::AgentServerMessage {
    let milliseconds = elapsed.as_millis().clamp(1, i32::MAX as u128) as i32;
    server_interaction(pb::interaction_update::Message::ThinkingCompleted(
        pb::ThinkingCompletedUpdate {
            thinking_duration_ms: milliseconds,
        },
    ))
}

pub fn arguments_delta(call: &ToolCall, delta: &str) -> Result<pb::AgentServerMessage> {
    Ok(server_interaction(
        pb::interaction_update::Message::PartialToolCall(pb::PartialToolCallUpdate {
            call_id: call.call_id.clone(),
            tool_call: Some(tool_placeholder(&call.name, &call.call_id)?),
            args_text_delta: delta.into(),
            model_call_id: call.model_call_id.clone(),
        }),
    ))
}

pub fn tool_started(call: &ToolCall) -> Result<pb::AgentServerMessage> {
    Ok(server_interaction(
        pb::interaction_update::Message::ToolCallStarted(pb::ToolCallStartedUpdate {
            call_id: call.call_id.clone(),
            tool_call: Some(render_tool_call(call, false)?),
            model_call_id: call.model_call_id.clone(),
        }),
    ))
}

pub fn tool_completed(call: &ToolCall, completion: &ToolCompletion) -> pb::AgentServerMessage {
    server_interaction(pb::interaction_update::Message::ToolCallCompleted(
        pb::ToolCallCompletedUpdate {
            call_id: call.call_id.clone(),
            tool_call: Some(completion.tool_call().clone()),
            model_call_id: call.model_call_id.clone(),
        },
    ))
}

pub fn turn_ended(usage: Usage) -> pb::AgentServerMessage {
    server_interaction(pb::interaction_update::Message::TurnEnded(
        pb::TurnEndedUpdate {
            input_tokens: Some(usage.input_tokens as i64),
            output_tokens: Some(usage.output_tokens as i64),
            cache_read_tokens: Some(usage.cache_read_tokens as i64),
            cache_write_tokens: Some(usage.cache_write_tokens as i64),
            reasoning_tokens: Some(usage.reasoning_tokens as i64),
        },
    ))
}

pub fn tool_query(id: u32, call: &ToolCall) -> Result<pb::AgentServerMessage> {
    use pb::interaction_query::Query;
    let string = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Protocol(format!("{} is missing {name}", call.name)))
    };
    let optional_string = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let query = match normalized(&call.name).as_str() {
        "askquestion" => {
            let questions = call
                .arguments
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|question| -> Result<_> {
                    let required = |name: &str| {
                        question
                            .get(name)
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .ok_or_else(|| Error::Protocol(format!("question is missing {name}")))
                    };
                    let options = question
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|option| -> Result<_> {
                            let value = |name: &str| {
                                option
                                    .get(name)
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .ok_or_else(|| {
                                        Error::Protocol(format!(
                                            "question option is missing {name}"
                                        ))
                                    })
                            };
                            Ok(pb::ask_question_args::Option {
                                id: value("id")?,
                                label: value("label")?,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(pb::ask_question_args::Question {
                        id: required("id")?,
                        prompt: required("prompt")?,
                        options,
                        allow_multiple: question
                            .get("allow_multiple")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Query::AskQuestionInteractionQuery(pb::AskQuestionInteractionQuery {
                args: Some(pb::AskQuestionArgs {
                    title: optional_string("title").unwrap_or_default(),
                    questions,
                    run_async: false,
                    async_original_tool_call_id: String::new(),
                }),
                tool_call_id: call.call_id.clone(),
            })
        }
        "websearch" => Query::WebSearchRequestQuery(pb::WebSearchRequestQuery {
            args: Some(pb::WebSearchArgs {
                search_term: string("search_term")?,
                tool_call_id: call.call_id.clone(),
            }),
        }),
        "webfetch" => Query::WebFetchRequestQuery(pb::WebFetchRequestQuery {
            args: Some(pb::WebFetchArgs {
                url: string("url")?,
                tool_call_id: call.call_id.clone(),
            }),
            skip_approval: false,
            smart_mode_approval: None,
        }),
        "switchmode" => Query::SwitchModeRequestQuery(pb::SwitchModeRequestQuery {
            args: Some(pb::SwitchModeArgs {
                target_mode_id: string("target_mode_id")?,
                explanation: optional_string("explanation"),
                tool_call_id: call.call_id.clone(),
            }),
        }),
        "createplan" => {
            let todos = call
                .arguments
                .get("todos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|todo| pb::TodoItem {
                    id: todo
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    content: todo
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    status: pb::TodoStatus::Pending as i32,
                    created_at: 0,
                    updated_at: 0,
                    dependencies: Vec::new(),
                })
                .collect();
            Query::CreatePlanRequestQuery(pb::CreatePlanRequestQuery {
                args: Some(pb::CreatePlanArgs {
                    plan: string("plan")?,
                    todos,
                    overview: string("overview")?,
                    name: string("name")?,
                    is_project: false,
                    phases: Vec::new(),
                }),
                tool_call_id: call.call_id.clone(),
            })
        }
        "generateimage" => Query::GenerateImageRequestQuery(pb::GenerateImageRequestQuery {
            args: Some(pb::GenerateImageArgs {
                description: optional_string("description").unwrap_or_default(),
                file_path: optional_string("file_path"),
                reference_image_paths: Vec::new(),
                aspect_ratio: optional_string("aspect_ratio"),
            }),
            tool_call_id: call.call_id.clone(),
        }),
        other => {
            return Err(Error::Protocol(format!(
                "tool {other} is not an InteractionQuery"
            )))
        }
    };
    Ok(pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::InteractionQuery(
            pb::InteractionQuery {
                id,
                query: Some(query),
            },
        )),
    })
}

pub fn server_interaction(message: pb::interaction_update::Message) -> pb::AgentServerMessage {
    pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::InteractionUpdate(
            pb::InteractionUpdate {
                message: Some(message),
            },
        )),
    }
}

pub fn tool_placeholder(name: &str, call_id: &str) -> Result<pb::ToolCall> {
    use pb::tool_call::Tool;
    let tool = match normalized(name).as_str() {
        "shell" | "forcebackgroundshell" => Tool::ShellToolCall(pb::ShellToolCall::default()),
        "delete" => Tool::DeleteToolCall(pb::DeleteToolCall::default()),
        "glob" => Tool::GlobToolCall(pb::GlobToolCall::default()),
        "grep" => Tool::GrepToolCall(pb::GrepToolCall::default()),
        "read" => Tool::ReadToolCall(pb::ReadToolCall::default()),
        "todowrite" => Tool::UpdateTodosToolCall(pb::UpdateTodosToolCall::default()),
        "patchedit" | "write" => Tool::EditToolCall(pb::EditToolCall::default()),
        "ls" => Tool::LsToolCall(pb::LsToolCall::default()),
        "readlints" => Tool::ReadLintsToolCall(pb::ReadLintsToolCall::default()),
        "callmcptool" => Tool::McpToolCall(pb::McpToolCall::default()),
        "createplan" => Tool::CreatePlanToolCall(pb::CreatePlanToolCall::default()),
        "websearch" => Tool::WebSearchToolCall(pb::WebSearchToolCall::default()),
        "task" => Tool::TaskToolCall(pb::TaskToolCall::default()),
        "fetchmcpresource" => Tool::ReadMcpResourceToolCall(pb::ReadMcpResourceToolCall::default()),
        "askquestion" => Tool::AskQuestionToolCall(pb::AskQuestionToolCall::default()),
        "webfetch" => Tool::WebFetchToolCall(pb::WebFetchToolCall::default()),
        "switchmode" => Tool::SwitchModeToolCall(pb::SwitchModeToolCall::default()),
        "generateimage" => Tool::GenerateImageToolCall(pb::GenerateImageToolCall::default()),
        "communicateupdate" => {
            Tool::CommunicateUpdateToolCall(pb::CommunicateUpdateToolCall::default())
        }
        "writeshellstdin" => Tool::WriteShellStdinToolCall(pb::WriteShellStdinToolCall::default()),
        _ => return Err(Error::Protocol(format!("unsupported tool: {name}"))),
    };
    Ok(pb::ToolCall {
        hook_additional_contexts: Vec::new(),
        tool_call_id: Some(call_id.into()),
        started_at_ms: None,
        completed_at_ms: None,
        tool: Some(tool),
    })
}

pub fn render_tool_call(call: &ToolCall, completed: bool) -> Result<pb::ToolCall> {
    let mut output = tool_placeholder(&call.name, &call.call_id)?;
    let timestamp = now_ms();
    output.started_at_ms = Some(timestamp);
    if completed {
        output.completed_at_ms = Some(timestamp);
    }
    let string = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let optional = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match output.tool.as_mut() {
        Some(pb::tool_call::Tool::ShellToolCall(tool)) => {
            tool.args = Some(pb::ShellArgs {
                command: string("command"),
                working_directory: optional("working_directory").unwrap_or_default(),
                tool_call_id: call.call_id.clone(),
                ..Default::default()
            })
        }
        Some(pb::tool_call::Tool::DeleteToolCall(tool)) => {
            tool.args = Some(pb::DeleteArgs {
                path: string("path"),
                tool_call_id: call.call_id.clone(),
            })
        }
        Some(pb::tool_call::Tool::GlobToolCall(tool)) => {
            tool.args = Some(pb::GlobToolArgs {
                target_directory: optional("target_directory"),
                glob_pattern: string("glob_pattern"),
            })
        }
        Some(pb::tool_call::Tool::GrepToolCall(tool)) => {
            tool.args = Some(pb::GrepArgs {
                pattern: string("pattern"),
                path: optional("path"),
                glob: optional("glob"),
                output_mode: optional("output_mode"),
                tool_call_id: call.call_id.clone(),
                ..Default::default()
            })
        }
        Some(pb::tool_call::Tool::ReadToolCall(tool)) => {
            tool.args = Some(pb::ReadToolArgs {
                path: string("path"),
                offset: call
                    .arguments
                    .get("offset")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                limit: call
                    .arguments
                    .get("limit")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                include_line_numbers: call
                    .arguments
                    .get("include_line_numbers")
                    .and_then(Value::as_bool),
            })
        }
        Some(pb::tool_call::Tool::UpdateTodosToolCall(tool)) => {
            tool.args = Some(pb::UpdateTodosArgs {
                todos: tool_result::todo_items(&call.arguments),
                merge: call
                    .arguments
                    .get("merge")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        Some(pb::tool_call::Tool::EditToolCall(tool)) => {
            let stream_content = if normalized(&call.name) == "write" {
                optional("contents").unwrap_or_default()
            } else {
                format!("{}\n---\n{}", string("old_string"), string("new_string"))
            };
            tool.args = Some(pb::EditArgs {
                path: string("path"),
                stream_content: Some(stream_content),
            })
        }
        Some(pb::tool_call::Tool::LsToolCall(tool)) => {
            tool.args = Some(pb::LsArgs {
                path: string("path"),
                tool_call_id: call.call_id.clone(),
                ..Default::default()
            })
        }
        Some(pb::tool_call::Tool::ReadLintsToolCall(tool)) => {
            tool.args = Some(pb::ReadLintsToolArgs {
                paths: call
                    .arguments
                    .get("paths")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            })
        }
        Some(pb::tool_call::Tool::McpToolCall(tool)) => {
            tool.args = Some(pb::McpArgs {
                name: optional("toolName").unwrap_or_default(),
                args: call
                    .arguments
                    .get("arguments")
                    .and_then(Value::as_object)
                    .map(super::exec::json_object_to_prost)
                    .unwrap_or_default(),
                tool_call_id: call.call_id.clone(),
                tool_name: optional("toolName").unwrap_or_default(),
                server_identifier: string("server"),
                ..Default::default()
            })
        }
        Some(pb::tool_call::Tool::CreatePlanToolCall(tool)) => {
            tool.args = Some(pb::CreatePlanArgs {
                plan: string("plan"),
                todos: tool_result::todo_items(&call.arguments),
                overview: string("overview"),
                name: string("name"),
                is_project: false,
                phases: Vec::new(),
            })
        }
        Some(pb::tool_call::Tool::WebSearchToolCall(tool)) => {
            tool.args = Some(pb::WebSearchArgs {
                search_term: string("search_term"),
                tool_call_id: call.call_id.clone(),
            })
        }
        Some(pb::tool_call::Tool::TaskToolCall(tool)) => {
            tool.args = Some(pb::TaskArgs {
                description: string("description"),
                prompt: string("prompt"),
                subagent_type: Some(subagent_type(&string("subagent_type"))),
                model: optional("model"),
                resume: optional("resume"),
                agent_id: None,
                attachments: Vec::new(),
                mode: 0,
                responding_to_message_ids: Vec::new(),
                environment: 0,
                machine: None,
            })
        }
        Some(pb::tool_call::Tool::ReadMcpResourceToolCall(tool)) => {
            tool.args = Some(pb::ReadMcpResourceExecArgs {
                server: string("server"),
                uri: string("uri"),
                download_path: optional("downloadPath"),
                tool_call_id: call.call_id.clone(),
                smart_mode_approval: None,
            })
        }
        Some(pb::tool_call::Tool::WebFetchToolCall(tool)) => {
            tool.args = Some(pb::WebFetchArgs {
                url: string("url"),
                tool_call_id: call.call_id.clone(),
            })
        }
        Some(pb::tool_call::Tool::SwitchModeToolCall(tool)) => {
            tool.args = Some(pb::SwitchModeArgs {
                target_mode_id: string("target_mode_id"),
                explanation: optional("explanation"),
                tool_call_id: call.call_id.clone(),
            })
        }
        Some(pb::tool_call::Tool::GenerateImageToolCall(tool)) => {
            tool.args = Some(pb::GenerateImageArgs {
                description: string("description"),
                file_path: optional("file_path"),
                reference_image_paths: Vec::new(),
                aspect_ratio: optional("aspect_ratio"),
            })
        }
        Some(pb::tool_call::Tool::CommunicateUpdateToolCall(tool)) => {
            tool.args = Some(pb::CommunicateUpdateArgs {
                current_step: optional("current_step"),
                final_summary: optional("final_summary"),
                completed_subtitle: optional("completed_subtitle"),
            })
        }
        Some(pb::tool_call::Tool::WriteShellStdinToolCall(tool)) => {
            tool.args = Some(pb::WriteShellStdinArgs {
                shell_id: call
                    .arguments
                    .get("shell_id")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u32,
                chars: string("chars"),
            })
        }
        _ => {}
    }
    Ok(output)
}

fn subagent_type(name: &str) -> pb::SubagentType {
    use pb::subagent_type::Type;
    let r#type = match name.to_ascii_lowercase().as_str() {
        "explore" => Type::Explore(pb::SubagentTypeExplore {}),
        "browser-use" | "browseruse" => Type::BrowserUse(pb::SubagentTypeBrowserUse {}),
        "shell" => Type::Shell(pb::SubagentTypeShell {}),
        "bash" => Type::Bash(pb::SubagentTypeBash {}),
        "debug" => Type::Debug(pb::SubagentTypeDebug {}),
        "computer-use" | "computeruse" => Type::ComputerUse(pb::SubagentTypeComputerUse {}),
        "" => Type::Unspecified(pb::SubagentTypeUnspecified {}),
        custom => Type::Custom(pb::SubagentTypeCustom {
            name: custom.into(),
        }),
    };
    pb::SubagentType {
        r#type: Some(r#type),
    }
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
