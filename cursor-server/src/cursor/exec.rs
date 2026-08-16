use serde_json::{Map, Value};

use crate::{
    cursor::{
        pending::{ExecContext, PendingExecRegistry},
        proto::agent::v1 as pb,
        tool_result::ToolCompletion,
    },
    model::ToolCall,
    Error, Result,
};

pub fn request(id: u32, call: &ToolCall, context: &ExecContext) -> Result<pb::AgentServerMessage> {
    use pb::exec_server_message::Message;
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
    let int = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_i64)
            .map(|v| v as i32)
    };
    let message = match normalize(&call.name).as_str() {
        "shell" => Message::ShellStreamArgs(pb::ShellArgs {
            command: string("command")?,
            working_directory: optional_string("working_directory").unwrap_or_default(),
            timeout: shell_timeout(call)?,
            tool_call_id: call.call_id.clone(),
            file_output_threshold_bytes: Some(40_000),
            timeout_behavior: pb::TimeoutBehavior::Background as i32,
            hard_timeout: Some(86_400_000),
            description: optional_string("description"),
            close_stdin: true,
            conversation_id: Some(context.conversation_id.clone()),
            admin_command_denylist: context.admin_command_denylist.clone(),
            ..Default::default()
        }),
        "forcebackgroundshell" => Message::ForceBackgroundShellArgs(pb::ForceBackgroundShellArgs {
            tool_call_id: string("tool_call_id")?,
        }),
        "read" => Message::ReadArgs(pb::ReadArgs {
            path: string("path")?,
            tool_call_id: call.call_id.clone(),
            offset: int("offset"),
            limit: call
                .arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            encoding_hint: optional_string("encoding_hint"),
        }),
        "write" => Message::WriteArgs(pb::WriteArgs {
            path: string("path")?,
            file_text: string("contents")?,
            tool_call_id: call.call_id.clone(),
            return_file_content_after_write: true,
            file_bytes: Vec::new(),
            encoding_hint: optional_string("encoding_hint"),
        }),
        "delete" => Message::DeleteArgs(pb::DeleteArgs {
            path: string("path")?,
            tool_call_id: call.call_id.clone(),
        }),
        "grep" => Message::GrepArgs(pb::GrepArgs {
            pattern: string("pattern")?,
            path: optional_string("path"),
            glob: optional_string("glob"),
            output_mode: optional_string("output_mode"),
            context_before: int("context_before"),
            context_after: int("context_after"),
            context: int("context"),
            case_insensitive: call
                .arguments
                .get("case_insensitive")
                .and_then(Value::as_bool),
            r#type: optional_string("type"),
            head_limit: int("head_limit"),
            multiline: call.arguments.get("multiline").and_then(Value::as_bool),
            sort: optional_string("sort"),
            sort_ascending: call
                .arguments
                .get("sort_ascending")
                .and_then(Value::as_bool),
            tool_call_id: call.call_id.clone(),
            sandbox_policy: None,
            offset: int("offset"),
        }),
        "glob" => Message::GrepArgs(pb::GrepArgs {
            pattern: String::new(),
            path: optional_string("target_directory"),
            glob: optional_string("glob_pattern"),
            output_mode: Some("files_with_matches".into()),
            tool_call_id: call.call_id.clone(),
            ..Default::default()
        }),
        "ls" => Message::LsArgs(pb::LsArgs {
            path: string("path")?,
            ignore: call
                .arguments
                .get("ignore")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            tool_call_id: call.call_id.clone(),
            sandbox_policy: None,
            timeout_ms: call
                .arguments
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
        }),
        "readlints" => Message::DiagnosticsArgs(pb::DiagnosticsArgs {
            path: call
                .arguments
                .get("paths")
                .and_then(Value::as_array)
                .and_then(|paths| paths.first())
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            tool_call_id: call.call_id.clone(),
        }),
        "patchedit" => Message::PiEditArgs(pb::PiEditExecArgs {
            path: string("path")?,
            edits: vec![pb::PiEditReplacement {
                old_text: string("old_string")?,
                new_text: string("new_string")?,
            }],
        }),
        "writeshellstdin" => Message::WriteShellStdinArgs(pb::WriteShellStdinArgs {
            shell_id: call
                .arguments
                .get("shell_id")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            chars: string("chars")?,
        }),
        "task" => Message::SubagentArgs(pb::SubagentArgs {
            tool_call_id: call.call_id.clone(),
            subagent_type: string("subagent_type")?,
            model_id: optional_string("model").unwrap_or_default(),
            prompt: string("prompt")?,
            readonly: call
                .arguments
                .get("readonly")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resume_agent_id: optional_string("resume"),
            run_in_background: Some(false),
            continuation_config: None,
            parent_conversation_id: None,
            interrupt: None,
            mode: 0,
            fork_agent_id: None,
            root_parent_conversation_id: None,
            selected_context: None,
            direct_meta_parent_child_subagent: None,
            environment: 0,
            cloud_base_branch: None,
            credentials: None,
        }),
        "callmcptool" => Message::McpArgs(pb::McpArgs {
            name: string("toolName")?,
            args: call
                .arguments
                .get("arguments")
                .and_then(Value::as_object)
                .map(json_object_to_prost)
                .unwrap_or_default(),
            tool_call_id: call.call_id.clone(),
            provider_identifier: optional_string("provider_identifier").unwrap_or_default(),
            tool_name: string("toolName")?,
            smart_mode_approval: None,
            smart_mode_approval_only: false,
            skip_approval: false,
            server_identifier: string("server")?,
        }),
        "fetchmcpresource" => Message::ReadMcpResourceExecArgs(pb::ReadMcpResourceExecArgs {
            server: string("server")?,
            uri: string("uri")?,
            download_path: optional_string("downloadPath"),
            tool_call_id: call.call_id.clone(),
            smart_mode_approval: None,
        }),
        "webfetch" => Message::FetchArgs(pb::FetchArgs {
            url: string("url")?,
            tool_call_id: call.call_id.clone(),
        }),
        other => {
            return Err(Error::Protocol(format!(
                "tool {other} is not executed through ExecServerMessage"
            )))
        }
    };
    Ok(pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::ExecServerMessage(
            pb::ExecServerMessage {
                id,
                exec_id: call.call_id.clone(),
                span_context: None,
                accept_hook_additional_contexts: Some(true),
                message: Some(message),
            },
        )),
    })
}

pub fn mcp_request(
    id: u32,
    call: &ToolCall,
    definition: &pb::McpToolDefinition,
) -> Result<pb::AgentServerMessage> {
    let args = call
        .arguments
        .as_object()
        .map(json_object_to_prost)
        .unwrap_or_default();
    Ok(pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::ExecServerMessage(
            pb::ExecServerMessage {
                id,
                exec_id: call.call_id.clone(),
                span_context: None,
                accept_hook_additional_contexts: None,
                message: Some(pb::exec_server_message::Message::McpArgs(pb::McpArgs {
                    name: definition.name.clone(),
                    args,
                    tool_call_id: call.call_id.clone(),
                    provider_identifier: definition.provider_identifier.clone(),
                    tool_name: if definition.tool_name.is_empty() {
                        definition.name.clone()
                    } else {
                        definition.tool_name.clone()
                    },
                    smart_mode_approval: None,
                    smart_mode_approval_only: false,
                    skip_approval: false,
                    server_identifier: String::new(),
                })),
            },
        )),
    })
}

pub fn abort(id: u32) -> pb::AgentServerMessage {
    pb::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(pb::agent_server_message::Message::ExecServerControlMessage(
            pb::ExecServerControlMessage {
                message: Some(pb::exec_server_control_message::Message::Abort(
                    pb::ExecServerAbort { id },
                )),
            },
        )),
    }
}

pub enum ClientExecEvent {
    Delta(Box<pb::AgentServerMessage>),
    Completed(Box<ToolCompletion>),
    Pending,
}

pub async fn client_event(
    message: &pb::ExecClientMessage,
    pending: &PendingExecRegistry,
) -> Result<ClientExecEvent> {
    let call = pending
        .call(message.id)
        .await
        .ok_or_else(|| Error::Protocol(format!("unknown ExecClientMessage id: {}", message.id)))?;
    let Some(wire_result) = &message.message else {
        return Ok(ClientExecEvent::Pending);
    };
    let pb::exec_client_message::Message::ShellStream(stream) = wire_result else {
        return complete(message.id, pending, wire_result.clone()).await;
    };
    use pb::shell_stream::Event;
    let event = match &stream.event {
        Some(Event::Stdout(stdout)) => {
            if pending.append_stdout(message.id, &stdout.data).await {
                ClientExecEvent::Delta(Box::new(shell_delta(&call, true, &stdout.data)))
            } else {
                ClientExecEvent::Pending
            }
        }
        Some(Event::Stderr(stderr)) => {
            if pending.append_stderr(message.id, &stderr.data).await {
                ClientExecEvent::Delta(Box::new(shell_delta(&call, false, &stderr.data)))
            } else {
                ClientExecEvent::Pending
            }
        }
        Some(Event::Start(_)) | Some(Event::HookContext(_)) => ClientExecEvent::Pending,
        Some(Event::Exit(exit)) => {
            let entry = take(message.id, pending).await?;
            let result = shell_exit_result(message, exit, &entry.stdout, &entry.stderr);
            completed(entry, pb::exec_client_message::Message::ShellResult(result))?
        }
        Some(Event::Backgrounded(backgrounded)) => {
            let entry = take(message.id, pending).await?;
            let result = shell_backgrounded_result(
                backgrounded,
                &entry.stdout,
                &entry.stderr,
                &entry.context.terminals_folder,
            );
            completed(entry, pb::exec_client_message::Message::ShellResult(result))?
        }
        Some(Event::Rejected(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::Rejected(value.clone())),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        Some(Event::PermissionDenied(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::PermissionDenied(value.clone())),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        Some(Event::SandboxUnsupported(value)) => {
            let result = pb::ShellResult {
                result: Some(pb::shell_result::Result::SpawnError(pb::ShellSpawnError {
                    command: value.command.clone(),
                    working_directory: value.working_directory.clone(),
                    error: value.reason.clone(),
                })),
                ..Default::default()
            };
            complete(
                message.id,
                pending,
                pb::exec_client_message::Message::ShellResult(result),
            )
            .await?
        }
        None => ClientExecEvent::Pending,
    };
    Ok(event)
}

async fn complete(
    id: u32,
    pending: &PendingExecRegistry,
    result: pb::exec_client_message::Message,
) -> Result<ClientExecEvent> {
    completed(take(id, pending).await?, result)
}

async fn take(id: u32, pending: &PendingExecRegistry) -> Result<super::pending::PendingExec> {
    pending
        .take(id)
        .await
        .ok_or_else(|| Error::Protocol(format!("unknown terminal Exec id: {id}")))
}

fn completed(
    pending: super::pending::PendingExec,
    result: pb::exec_client_message::Message,
) -> Result<ClientExecEvent> {
    Ok(ClientExecEvent::Completed(Box::new(
        super::tool_result::from_exec(pending, &result)?,
    )))
}

fn shell_exit_result(
    message: &pb::ExecClientMessage,
    exit: &pb::ShellStreamExit,
    stdout: &str,
    stderr: &str,
) -> pb::ShellResult {
    let result = if exit.code == 0 && !exit.aborted {
        pb::shell_result::Result::Success(pb::ShellSuccess {
            working_directory: exit.cwd.clone(),
            exit_code: exit.code as i32,
            stdout: stdout.into(),
            stderr: stderr.into(),
            interleaved_output: Some(format!("{stdout}{stderr}")),
            local_execution_time_ms: exit
                .local_execution_time_ms
                .or(message.local_execution_time_ms),
            ..Default::default()
        })
    } else {
        pb::shell_result::Result::Failure(pb::ShellFailure {
            working_directory: exit.cwd.clone(),
            exit_code: exit.code as i32,
            stdout: stdout.into(),
            stderr: stderr.into(),
            interleaved_output: Some(format!("{stdout}{stderr}")),
            abort_reason: exit.abort_reason,
            aborted: exit.aborted,
            local_execution_time_ms: exit
                .local_execution_time_ms
                .or(message.local_execution_time_ms),
            ..Default::default()
        })
    };
    pb::ShellResult {
        result: Some(result),
        is_background: Some(false),
        ..Default::default()
    }
}

fn shell_backgrounded_result(
    backgrounded: &pb::ShellStreamBackgrounded,
    stdout: &str,
    stderr: &str,
    terminals_folder: &str,
) -> pb::ShellResult {
    pb::ShellResult {
        result: Some(pb::shell_result::Result::Success(pb::ShellSuccess {
            command: backgrounded.command.clone(),
            working_directory: backgrounded.working_directory.clone(),
            stdout: stdout.into(),
            stderr: stderr.into(),
            shell_id: Some(backgrounded.shell_id),
            pid: backgrounded.pid,
            ms_to_wait: backgrounded.ms_to_wait,
            background_reason: backgrounded.reason,
            interleaved_output: Some(format!("{stdout}{stderr}")),
            ..Default::default()
        })),
        is_background: Some(true),
        terminals_folder: (!terminals_folder.is_empty()).then(|| terminals_folder.into()),
        pid: backgrounded.pid,
        ..Default::default()
    }
}

fn shell_delta(call: &ToolCall, stdout: bool, content: &str) -> pb::AgentServerMessage {
    let delta = if stdout {
        pb::shell_tool_call_delta::Delta::Stdout(pb::ShellToolCallStdoutDelta {
            content: content.into(),
        })
    } else {
        pb::shell_tool_call_delta::Delta::Stderr(pb::ShellToolCallStderrDelta {
            content: content.into(),
        })
    };
    super::interaction::server_interaction(pb::interaction_update::Message::ToolCallDelta(
        Box::new(pb::ToolCallDeltaUpdate {
            call_id: call.call_id.clone(),
            tool_call_delta: Some(Box::new(pb::ToolCallDelta {
                delta: Some(pb::tool_call_delta::Delta::ShellToolCallDelta(
                    pb::ShellToolCallDelta { delta: Some(delta) },
                )),
            })),
            model_call_id: call.model_call_id.clone(),
        }),
    ))
}

fn shell_timeout(call: &ToolCall) -> Result<i32> {
    let value = call
        .arguments
        .get("block_until_ms")
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| Error::Protocol("Shell block_until_ms must be an integer".into()))
        })
        .transpose()?
        .unwrap_or(30_000);
    i32::try_from(value)
        .ok()
        .filter(|value| *value >= 0)
        .ok_or_else(|| Error::Protocol("Shell block_until_ms is out of range".into()))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn json_object_to_prost(
    value: &Map<String, Value>,
) -> std::collections::HashMap<String, prost_types::Value> {
    value
        .iter()
        .map(|(key, value)| (key.clone(), prost_value(value)))
        .collect()
}

fn prost_value(value: &Value) -> prost_types::Value {
    use prost_types::{value::Kind, ListValue, Struct, Value as ProstValue};
    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(v) => Kind::BoolValue(*v),
        Value::Number(v) => Kind::NumberValue(v.as_f64().unwrap_or_default()),
        Value::String(v) => Kind::StringValue(v.clone()),
        Value::Array(v) => Kind::ListValue(ListValue {
            values: v.iter().map(prost_value).collect(),
        }),
        Value::Object(v) => Kind::StructValue(Struct {
            fields: json_object_to_prost(v).into_iter().collect(),
        }),
    };
    ProstValue { kind: Some(kind) }
}
