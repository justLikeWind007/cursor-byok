use tokio::sync::mpsc;

use crate::{
    cursor::{
        interaction,
        pending::{now_ms, PendingClientTool, PendingExec},
        proto::agent::v1 as pb,
    },
    model::{ToolCall, ToolResult},
    Error, Result,
};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct ToolCompletion {
    result: ToolResult,
    tool_call: pb::ToolCall,
}

impl ToolCompletion {
    pub fn result(&self) -> &ToolResult {
        &self.result
    }

    pub fn tool_call(&self) -> &pb::ToolCall {
        &self.tool_call
    }

    pub(crate) fn new(
        call: &ToolCall,
        started_at_ms: u64,
        result: ToolResult,
        tool: pb::tool_call::Tool,
    ) -> Self {
        Self {
            result,
            tool_call: pb::ToolCall {
                tool_call_id: Some(call.call_id.clone()),
                started_at_ms: Some(started_at_ms),
                completed_at_ms: Some(now_ms()),
                tool: Some(tool),
                hook_additional_contexts: Vec::new(),
            },
        }
    }

    fn from_rendered(
        call: &ToolCall,
        started_at_ms: u64,
        output: String,
        is_error: bool,
        rendered: pb::ToolCall,
    ) -> Result<Self> {
        let tool = rendered.tool.ok_or_else(|| {
            Error::Protocol(format!("tool {} has no Cursor representation", call.name))
        })?;
        Ok(Self::new(
            call,
            started_at_ms,
            ToolResult {
                call_id: call.call_id.clone(),
                output: Value::String(output),
                is_error,
            },
            tool,
        ))
    }
}

#[derive(Clone)]
pub struct ToolResultSender(mpsc::UnboundedSender<Result<ToolCompletion>>);
pub struct ToolResultReceiver(mpsc::UnboundedReceiver<Result<ToolCompletion>>);

pub fn tool_result_channel() -> (ToolResultSender, ToolResultReceiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (ToolResultSender(sender), ToolResultReceiver(receiver))
}

impl ToolResultSender {
    pub fn send(&self, result: ToolCompletion) {
        let _ = self.0.send(Ok(result));
    }

    pub fn send_error(&self, error: Error) {
        let _ = self.0.send(Err(error));
    }
}

impl ToolResultReceiver {
    pub async fn recv(&mut self) -> Option<Result<ToolCompletion>> {
        self.0.recv().await
    }
}

pub(crate) fn from_exec(
    pending: PendingExec,
    wire_result: &pb::exec_client_message::Message,
) -> Result<ToolCompletion> {
    use pb::{exec_client_message::Message, tool_call::Tool};
    let call = &pending.call;
    let (output, is_error) = exec_output(wire_result)?;
    let mut tool_call = interaction::render_tool_call(call, false)?;
    match (tool_call.tool.as_mut(), wire_result) {
        (Some(Tool::ShellToolCall(tool)), Message::ShellResult(result))
        | (Some(Tool::ShellToolCall(tool)), Message::MiniSweAgentBashResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::ShellToolCall(tool)), Message::ForceBackgroundShellResult(result)) => {
            tool.result = result.shell_result.clone();
        }
        (Some(Tool::DeleteToolCall(tool)), Message::DeleteResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::GrepToolCall(tool)), Message::GrepResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::GlobToolCall(tool)), Message::GrepResult(result)) => {
            tool.result = Some(glob_result(result));
        }
        (Some(Tool::ReadToolCall(tool)), Message::ReadResult(result))
        | (Some(Tool::ReadToolCall(tool)), Message::RedactedReadResult(result)) => {
            tool.result = Some(read_tool_result(result, call));
        }
        (Some(Tool::LsToolCall(tool)), Message::LsResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::ReadLintsToolCall(tool)), Message::DiagnosticsResult(result)) => {
            tool.result = Some(read_lints_result(result));
        }
        (Some(Tool::McpToolCall(tool)), Message::McpResult(result)) => {
            tool.result = Some(mcp_tool_result(result));
        }
        (Some(Tool::ReadMcpResourceToolCall(tool)), Message::ReadMcpResourceExecResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::WebFetchToolCall(tool)), Message::FetchResult(result)) => {
            tool.result = Some(web_fetch_result(result));
        }
        (Some(Tool::TaskToolCall(tool)), Message::SubagentResult(result)) => {
            tool.result = Some(task_result(result));
        }
        (Some(Tool::WriteShellStdinToolCall(tool)), Message::WriteShellStdinResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::EditToolCall(tool)), Message::WriteResult(result)) => {
            tool.result = Some(write_result(result));
        }
        (Some(Tool::EditToolCall(tool)), Message::PiEditResult(result)) => {
            tool.result = Some(edit_result(result, call));
        }
        _ => {
            return Err(Error::Protocol(format!(
                "unexpected Exec result for tool {}",
                call.name
            )))
        }
    }
    let tool = tool_call.tool.ok_or_else(|| {
        Error::Protocol(format!("tool {} has no Cursor representation", call.name))
    })?;
    Ok(ToolCompletion::new(
        call,
        pending.started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            output: Value::String(output),
            is_error,
        },
        tool,
    ))
}

pub(crate) fn local(call: &ToolCall, message_index: usize) -> Result<ToolCompletion> {
    match normalize(&call.name).as_str() {
        "todowrite" => todo_write(call),
        "communicateupdate" => communicate_update(call, message_index),
        _ => Err(Error::Protocol(format!("unsupported tool: {}", call.name))),
    }
}

fn todo_write(call: &ToolCall) -> Result<ToolCompletion> {
    let todos = todo_items(&call.arguments);
    let total_count = todos.len() as i32;
    let was_merge = call
        .arguments
        .get("merge")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut rendered = interaction::render_tool_call(call, false)?;
    let Some(pb::tool_call::Tool::UpdateTodosToolCall(tool)) = rendered.tool.as_mut() else {
        return Err(Error::Protocol(
            "TodoWrite has no Cursor representation".into(),
        ));
    };
    tool.result = Some(pb::UpdateTodosResult {
        result: Some(pb::update_todos_result::Result::Success(
            pb::UpdateTodosSuccess {
                todos,
                total_count,
                was_merge,
            },
        )),
    });
    let tool = rendered
        .tool
        .ok_or_else(|| Error::Protocol("TodoWrite has no Cursor representation".into()))?;
    Ok(ToolCompletion::new(
        call,
        now_ms(),
        ToolResult {
            call_id: call.call_id.clone(),
            output: call.arguments.clone(),
            is_error: false,
        },
        tool,
    ))
}

fn communicate_update(call: &ToolCall, message_index: usize) -> Result<ToolCompletion> {
    let current_step = call
        .arguments
        .get("current_step")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut rendered = interaction::render_tool_call(call, false)?;
    let Some(pb::tool_call::Tool::CommunicateUpdateToolCall(tool)) = rendered.tool.as_mut() else {
        return Err(Error::Protocol(
            "CommunicateUpdate has no Cursor representation".into(),
        ));
    };
    let message_index = u32::try_from(message_index)
        .map_err(|_| Error::Protocol("Cursor message index space exhausted".into()))?;
    tool.result = Some(pb::CommunicateUpdateResult {
        result: Some(pb::communicate_update_result::Result::Success(
            pb::CommunicateUpdateSuccess {
                current_step: current_step.clone(),
                message_index,
            },
        )),
    });
    let tool = rendered
        .tool
        .ok_or_else(|| Error::Protocol("CommunicateUpdate has no Cursor representation".into()))?;
    Ok(ToolCompletion::new(
        call,
        now_ms(),
        ToolResult {
            call_id: call.call_id.clone(),
            output: serde_json::json!({
                "success": {
                    "current_step": current_step,
                    "message_index": message_index,
                }
            }),
            is_error: false,
        },
        tool,
    ))
}

pub(crate) fn from_interaction(
    pending: PendingClientTool,
    response: &pb::InteractionResponse,
) -> Result<ToolCompletion> {
    use pb::{interaction_response::Result as Response, tool_call::Tool};
    let call = &pending.call;
    let mut rendered = interaction::render_tool_call(call, false)?;
    let (output, is_error) = match (rendered.tool.as_mut(), response.result.as_ref()) {
        (
            Some(Tool::AskQuestionToolCall(tool)),
            Some(Response::AskQuestionInteractionResponse(value)),
        ) => {
            let result = value
                .result
                .clone()
                .ok_or_else(|| missing("ask question"))?;
            let output = ask_output(&result)?;
            tool.result = Some(result);
            output
        }
        (
            Some(Tool::CreatePlanToolCall(tool)),
            Some(Response::CreatePlanRequestResponse(value)),
        ) => {
            let result = value.result.clone().ok_or_else(|| missing("create plan"))?;
            let output = create_plan_output(&result)?;
            tool.result = Some(result);
            output
        }
        (
            Some(Tool::SwitchModeToolCall(tool)),
            Some(Response::SwitchModeRequestResponse(value)),
        ) => {
            let (result, output) = switch_mode_result(value)?;
            tool.result = Some(result);
            output
        }
        (Some(Tool::WebSearchToolCall(tool)), Some(Response::WebSearchRequestResponse(value))) => {
            match value
                .result
                .as_ref()
                .ok_or_else(|| missing("web search approval"))?
            {
                pb::web_search_request_response::Result::Rejected(rejected) => {
                    tool.result = Some(pb::WebSearchResult {
                        result: Some(pb::web_search_result::Result::Rejected(
                            pb::WebSearchRejected {
                                reason: rejected.reason.clone(),
                            },
                        )),
                    });
                    (rejected.reason.clone(), true)
                }
                pb::web_search_request_response::Result::Approved(_) => {
                    return Err(Error::Protocol(
                        "WebSearch approval is not a terminal tool result".into(),
                    ))
                }
            }
        }
        (Some(Tool::WebFetchToolCall(tool)), Some(Response::WebFetchRequestResponse(value))) => {
            match value
                .result
                .as_ref()
                .ok_or_else(|| missing("web fetch approval"))?
            {
                pb::web_fetch_request_response::Result::Rejected(rejected) => {
                    tool.result = Some(pb::WebFetchResult {
                        result: Some(pb::web_fetch_result::Result::Rejected(
                            pb::WebFetchRejected {
                                reason: rejected.reason.clone(),
                            },
                        )),
                    });
                    (rejected.reason.clone(), true)
                }
                pb::web_fetch_request_response::Result::Approved(_) => {
                    return Err(Error::Protocol(
                        "WebFetch approval is not a terminal tool result".into(),
                    ))
                }
            }
        }
        (
            Some(Tool::GenerateImageToolCall(tool)),
            Some(Response::GenerateImageRequestResponse(value)),
        ) => match value
            .result
            .as_ref()
            .ok_or_else(|| missing("generate image approval"))?
        {
            pb::generate_image_request_response::Result::Rejected(rejected) => {
                tool.result = Some(pb::GenerateImageResult {
                    result: Some(pb::generate_image_result::Result::Error(
                        pb::GenerateImageError {
                            error: rejected.reason.clone(),
                        },
                    )),
                });
                (rejected.reason.clone(), true)
            }
            pb::generate_image_request_response::Result::Approved(_) => {
                return Err(Error::Protocol(
                    "GenerateImage approval is not a terminal tool result".into(),
                ))
            }
        },
        _ => {
            return Err(Error::Protocol(format!(
                "unexpected InteractionResponse for tool {}",
                call.name
            )))
        }
    };
    ToolCompletion::from_rendered(call, pending.started_at_ms, output, is_error, rendered)
}

fn ask_output(value: &pb::AskQuestionResult) -> Result<(String, bool)> {
    use pb::ask_question_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("ask question"))?
    {
        R::Success(v) => Ok((
            v.answers
                .iter()
                .map(|answer| {
                    let value = if answer.freeform_text.is_empty() {
                        answer.selected_option_ids.join(", ")
                    } else {
                        answer.freeform_text.clone()
                    };
                    format!("{}: {value}", answer.question_id)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        )),
        R::Error(v) => Ok((v.error_message.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::Async(_) => Ok(("question is running asynchronously".into(), false)),
    }
}

fn create_plan_output(value: &pb::CreatePlanResult) -> Result<(String, bool)> {
    use pb::create_plan_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("create plan"))?
    {
        R::Success(_) => Ok((format!("plan created: {}", value.plan_uri), false)),
        R::Error(v) => Ok((v.error.clone(), true)),
    }
}

fn switch_mode_result(
    value: &pb::SwitchModeRequestResponse,
) -> Result<(pb::SwitchModeResult, (String, bool))> {
    use pb::{switch_mode_request_response::Result as Input, switch_mode_result::Result as Output};
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("switch mode"))?
    {
        Input::Approved(_) => Ok((
            pb::SwitchModeResult {
                result: Some(Output::Success(pb::SwitchModeSuccess::default())),
            },
            ("mode switched".into(), false),
        )),
        Input::Rejected(v) => Ok((
            pb::SwitchModeResult {
                result: Some(Output::Rejected(pb::SwitchModeRejected {
                    reason: v.reason.clone(),
                })),
            },
            (v.reason.clone(), true),
        )),
    }
}

fn exec_output(message: &pb::exec_client_message::Message) -> Result<(String, bool)> {
    use pb::exec_client_message::Message;
    match message {
        Message::ShellResult(value) | Message::MiniSweAgentBashResult(value) => shell_output(value),
        Message::ForceBackgroundShellResult(value) => value
            .shell_result
            .as_ref()
            .ok_or_else(|| missing("force background shell"))
            .and_then(shell_output),
        Message::ReadResult(value) | Message::RedactedReadResult(value) => read_output(value),
        Message::WriteResult(value) => write_output(value),
        Message::DeleteResult(value) => delete_output(value),
        Message::GrepResult(value) => grep_output(value),
        Message::LsResult(value) => ls_output(value),
        Message::DiagnosticsResult(value) => diagnostics_output(value),
        Message::McpResult(value) => mcp_output(value),
        Message::ReadMcpResourceExecResult(value) => read_mcp_output(value),
        Message::FetchResult(value) => fetch_output(value),
        Message::SubagentResult(value) => task_output(value),
        Message::WriteShellStdinResult(value) => write_stdin_output(value),
        Message::PiEditResult(value) => edit_output(value),
        _ => Err(Error::Protocol(
            "unsupported terminal ExecClientMessage".into(),
        )),
    }
}

fn shell_output(value: &pb::ShellResult) -> Result<(String, bool)> {
    use pb::shell_result::Result as R;
    let output = match value.result.as_ref().ok_or_else(|| missing("shell"))? {
        R::Success(v) if value.is_background == Some(true) => {
            let mut fields = vec![format!("shell_id={}", v.shell_id.unwrap_or_default())];
            if let Some(pid) = v.pid.or(value.pid) {
                fields.push(format!("pid={pid}"));
            }
            if let Some(folder) = value.terminals_folder.as_deref().filter(|v| !v.is_empty()) {
                fields.push(format!("terminals_folder={folder}"));
            }
            let output = streams(&v.stdout, &v.stderr);
            if output == "shell completed without output" {
                return Ok((
                    format!("shell running in background {}", fields.join(" ")),
                    false,
                ));
            }
            return Ok((
                format!("shell running in background {}\n{output}", fields.join(" ")),
                false,
            ));
        }
        R::Success(v) => return Ok((streams(&v.stdout, &v.stderr), false)),
        R::Failure(v) => streams(&v.stdout, &v.stderr),
        R::Timeout(v) => format!(
            "shell timed out after {}ms in {}",
            v.timeout_ms, v.working_directory
        ),
        R::Rejected(v) => v.reason.clone(),
        R::SpawnError(v) => v.error.clone(),
        R::PermissionDenied(v) => v.error.clone(),
    };
    Ok((output, true))
}

fn streams(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n\n<stderr>\n{stderr}\n</stderr>"),
        (false, true) => stdout.into(),
        (true, false) => stderr.into(),
        (true, true) => "shell completed without output".into(),
    }
}

fn read_output(value: &pb::ReadResult) -> Result<(String, bool)> {
    use pb::{read_result::Result as R, read_success::Output};
    match value.result.as_ref().ok_or_else(|| missing("read"))? {
        R::Success(v) => Ok((
            match v.output.as_ref() {
                Some(Output::Content(text)) => text.clone(),
                Some(Output::Data(bytes)) => format!("read binary bytes={}", bytes.len()),
                None => format!("read success path={}", v.path),
            },
            false,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::FileNotFound(v) => Ok((format!("file not found: {}", v.path), true)),
        R::PermissionDenied(v) => Ok((format!("permission denied: {}", v.path), true)),
        R::InvalidFile(v) => Ok((v.reason.clone(), true)),
    }
}

fn write_output(value: &pb::WriteResult) -> Result<(String, bool)> {
    use pb::write_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("write"))? {
        R::Success(v) => Ok((
            v.file_content_after_write.clone().unwrap_or_else(|| {
                format!("write success path={} lines={}", v.path, v.lines_created)
            }),
            false,
        )),
        R::PermissionDenied(v) => Ok((v.error.clone(), true)),
        R::NoSpace(v) => Ok((format!("no space left: {}", v.path), true)),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
    }
}

fn delete_output(value: &pb::DeleteResult) -> Result<(String, bool)> {
    use pb::delete_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("delete"))? {
        R::Success(v) => Ok((format!("delete success path={}", v.path), false)),
        R::FileNotFound(v) => Ok((format!("file not found: {}", v.path), true)),
        R::NotFile(v) => Ok((format!("not file: {}", v.path), true)),
        R::PermissionDenied(v) => Ok((v.client_visible_error.clone(), true)),
        R::FileBusy(v) => Ok((format!("file busy: {}", v.path), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::Error(v) => Ok((v.error.clone(), true)),
    }
}

fn grep_output(value: &pb::GrepResult) -> Result<(String, bool)> {
    use pb::grep_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("grep"))? {
        R::Success(v) => Ok((
            format!("grep success pattern={} mode={}", v.pattern, v.output_mode),
            false,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
    }
}

fn ls_output(value: &pb::LsResult) -> Result<(String, bool)> {
    use pb::ls_result::Result as R;
    let tree = |root: &Option<pb::LsDirectoryTreeNode>| {
        root.as_ref()
            .map(|v| format!("path={} files={}", v.abs_path, v.num_files))
            .unwrap_or_else(|| "empty directory".into())
    };
    match value.result.as_ref().ok_or_else(|| missing("ls"))? {
        R::Success(v) => Ok((
            format!("ls success {}", tree(&v.directory_tree_root)),
            false,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::Timeout(v) => Ok((format!("ls timeout {}", tree(&v.directory_tree_root)), true)),
    }
}

fn diagnostics_output(value: &pb::DiagnosticsResult) -> Result<(String, bool)> {
    use pb::diagnostics_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("diagnostics"))?
    {
        R::Success(v) => Ok((
            format!("diagnostics path={} count={}", v.path, v.total_diagnostics),
            false,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::FileNotFound(v) => Ok((format!("file not found: {}", v.path), true)),
        R::PermissionDenied(v) => Ok((format!("permission denied: {}", v.path), true)),
    }
}

fn mcp_output(value: &pb::McpResult) -> Result<(String, bool)> {
    use pb::mcp_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("mcp"))? {
        R::Success(v) => Ok((
            format!("mcp success content={}", v.content.len()),
            v.is_error,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::PermissionDenied(v) => Ok((v.error.clone(), true)),
        R::ToolNotFound(v) => Ok((format!("MCP tool not found: {}", v.name), true)),
        R::ServerNotFound(v) => Ok((format!("MCP server not found: {}", v.name), true)),
        R::Approved(_) => Err(Error::Protocol("MCP approval is not terminal".into())),
    }
}

fn read_mcp_output(value: &pb::ReadMcpResourceExecResult) -> Result<(String, bool)> {
    use pb::read_mcp_resource_exec_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("read MCP resource"))?
    {
        R::Success(v) => Ok((
            match v.content.as_ref() {
                Some(pb::read_mcp_resource_success::Content::Text(text)) => text.clone(),
                Some(pb::read_mcp_resource_success::Content::Blob(blob)) => {
                    format!("read MCP resource blob={}", blob.len())
                }
                None => format!("read MCP resource uri={}", v.uri),
            },
            false,
        )),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
        R::NotFound(v) => Ok((format!("MCP resource not found: {}", v.uri), true)),
    }
}

fn fetch_output(value: &pb::FetchResult) -> Result<(String, bool)> {
    use pb::fetch_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("web fetch"))? {
        R::Success(success) => Ok((success.content.clone(), false)),
        R::Error(error) => Ok((error.error.clone(), true)),
    }
}

fn web_fetch_result(value: &pb::FetchResult) -> pb::WebFetchResult {
    let result = match value
        .result
        .as_ref()
        .expect("exec_output validated WebFetch result")
    {
        pb::fetch_result::Result::Success(success) => {
            pb::web_fetch_result::Result::Success(pb::WebFetchSuccess {
                url: success.url.clone(),
                markdown: success.content.clone(),
                output_location: None,
            })
        }
        pb::fetch_result::Result::Error(error) => {
            pb::web_fetch_result::Result::Error(pb::WebFetchError {
                url: error.url.clone(),
                error: error.error.clone(),
            })
        }
    };
    pb::WebFetchResult {
        result: Some(result),
    }
}

fn task_output(value: &pb::SubagentResult) -> Result<(String, bool)> {
    use pb::subagent_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("subagent"))? {
        R::Success(v) => Ok((v.final_message.clone().unwrap_or_default(), false)),
        R::Error(v) => Ok((v.error.clone(), true)),
    }
}

fn write_stdin_output(value: &pb::WriteShellStdinResult) -> Result<(String, bool)> {
    use pb::write_shell_stdin_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("write shell stdin"))?
    {
        R::Success(v) => Ok((format!("wrote input to shell {}", v.shell_id), false)),
        R::Error(v) => Ok((v.error.clone(), true)),
    }
}

fn edit_output(value: &pb::PiEditExecResult) -> Result<(String, bool)> {
    use pb::pi_edit_exec_result::Result as R;
    match value.result.as_ref().ok_or_else(|| missing("edit"))? {
        R::Success(v) => Ok((v.output.clone(), false)),
        R::Error(v) => Ok((v.error.clone(), true)),
        R::Rejected(v) => Ok((v.reason.clone(), true)),
    }
}

fn missing(name: &str) -> Error {
    Error::Protocol(format!("{name} returned no result"))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn argument_string(call: &ToolCall, name: &str) -> String {
    call.arguments
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn read_tool_result(result: &pb::ReadResult, call: &ToolCall) -> pb::ReadToolResult {
    use pb::{read_result::Result as Input, read_tool_result::Result as Output};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => Output::Success(pb::ReadToolSuccess {
            is_empty: match success.output.as_ref() {
                Some(pb::read_success::Output::Content(content)) => content.is_empty(),
                Some(pb::read_success::Output::Data(data)) => data.is_empty(),
                None => true,
            },
            exceeded_limit: success.truncated,
            total_lines: success.total_lines.max(0) as u32,
            file_size: success.file_size.max(0).min(u32::MAX as i64) as u32,
            path: success.path.clone(),
            read_range: read_range(call),
            include_line_numbers: call
                .arguments
                .get("include_line_numbers")
                .and_then(Value::as_bool),
            output: success.output.as_ref().map(|output| match output {
                pb::read_success::Output::Content(content) => {
                    pb::read_tool_success::Output::Content(content.clone())
                }
                pb::read_success::Output::Data(data) => {
                    pb::read_tool_success::Output::Data(data.clone())
                }
            }),
            ..Default::default()
        }),
        Some(Input::Error(error)) => Output::Error(pb::ReadToolError {
            error_message: error.error.clone(),
        }),
        Some(Input::Rejected(rejected)) => Output::Error(pb::ReadToolError {
            error_message: rejected.reason.clone(),
        }),
        Some(Input::FileNotFound(not_found)) => Output::Error(pb::ReadToolError {
            error_message: format!("file not found: {}", not_found.path),
        }),
        Some(Input::PermissionDenied(denied)) => Output::Error(pb::ReadToolError {
            error_message: format!("permission denied: {}", denied.path),
        }),
        Some(Input::InvalidFile(invalid)) => Output::Error(pb::ReadToolError {
            error_message: invalid.reason.clone(),
        }),
        None => unreachable!("exec_output validated the read result"),
    };
    pb::ReadToolResult {
        result: Some(result),
    }
}

fn read_range(call: &ToolCall) -> Option<pb::ReadRange> {
    let start_line = call
        .arguments
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let limit = call
        .arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as u32)?;
    Some(pb::ReadRange {
        start_line,
        end_line: start_line.saturating_add(limit),
    })
}

fn write_result(result: &pb::WriteResult) -> pb::EditResult {
    use pb::{edit_result::Result as Output, write_result::Result as Input};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => Output::Success(pb::EditSuccess {
            path: success.path.clone(),
            after_full_file_content: success.file_content_after_write.clone().unwrap_or_default(),
            ..Default::default()
        }),
        Some(Input::PermissionDenied(denied)) => {
            Output::WritePermissionDenied(pb::EditWritePermissionDenied {
                path: denied.path.clone(),
                error: denied.error.clone(),
                is_readonly: denied.is_readonly,
            })
        }
        Some(Input::NoSpace(no_space)) => Output::Error(pb::EditError {
            path: no_space.path.clone(),
            error: "no space left".into(),
            model_visible_error: Some("no space left".into()),
        }),
        Some(Input::Error(error)) => Output::Error(pb::EditError {
            path: error.path.clone(),
            error: error.error.clone(),
            model_visible_error: Some(error.error.clone()),
        }),
        Some(Input::Rejected(rejected)) => Output::Rejected(pb::EditRejected {
            path: rejected.path.clone(),
            reason: rejected.reason.clone(),
        }),
        None => unreachable!("exec_output validated the write result"),
    };
    pb::EditResult {
        result: Some(result),
    }
}

fn edit_result(result: &pb::PiEditExecResult, call: &ToolCall) -> pb::EditResult {
    use pb::{edit_result::Result as Output, pi_edit_exec_result::Result as Input};
    let path = argument_string(call, "path");
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => Output::Success(pb::EditSuccess {
            path,
            diff_string: Some(success.diff.clone()),
            message: Some(success.output.clone()),
            ..Default::default()
        }),
        Some(Input::Error(error)) => Output::Error(pb::EditError {
            path,
            error: error.error.clone(),
            model_visible_error: Some(error.error.clone()),
        }),
        Some(Input::Rejected(rejected)) => Output::Rejected(pb::EditRejected {
            path,
            reason: rejected.reason.clone(),
        }),
        None => unreachable!("exec_output validated the edit result"),
    };
    pb::EditResult {
        result: Some(result),
    }
}

fn read_lints_result(result: &pb::DiagnosticsResult) -> pb::ReadLintsToolResult {
    use pb::{diagnostics_result::Result as Input, read_lints_tool_result::Result as Output};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => {
            let diagnostics = success
                .diagnostics
                .iter()
                .map(|diagnostic| pb::DiagnosticItem {
                    severity: diagnostic.severity,
                    range: diagnostic.range.as_ref().map(|range| pb::DiagnosticRange {
                        start: range.start,
                        end: range.end,
                    }),
                    message: diagnostic.message.clone(),
                    source: diagnostic.source.clone(),
                    code: diagnostic.code.clone(),
                    is_stale: diagnostic.is_stale,
                })
                .collect::<Vec<_>>();
            Output::Success(pb::ReadLintsToolSuccess {
                file_diagnostics: vec![pb::FileDiagnostics {
                    path: success.path.clone(),
                    diagnostics_count: diagnostics.len() as i32,
                    diagnostics,
                }],
                total_files: 1,
                total_diagnostics: success.total_diagnostics,
            })
        }
        Some(Input::Error(error)) => Output::Error(pb::ReadLintsToolError {
            error_message: error.error.clone(),
        }),
        Some(Input::Rejected(rejected)) => Output::Error(pb::ReadLintsToolError {
            error_message: rejected.reason.clone(),
        }),
        Some(Input::FileNotFound(not_found)) => Output::Error(pb::ReadLintsToolError {
            error_message: format!("file not found: {}", not_found.path),
        }),
        Some(Input::PermissionDenied(denied)) => Output::Error(pb::ReadLintsToolError {
            error_message: format!("permission denied: {}", denied.path),
        }),
        None => unreachable!("exec_output validated the diagnostics result"),
    };
    pb::ReadLintsToolResult {
        result: Some(result),
    }
}

fn mcp_tool_result(result: &pb::McpResult) -> pb::McpToolResult {
    use pb::{mcp_result::Result as Input, mcp_tool_result::Result as Output};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => Output::Success(success.clone()),
        Some(Input::Error(error)) => Output::Error(pb::McpToolError {
            error: error.error.clone(),
            read_tool_def_reminder: String::new(),
        }),
        Some(Input::Rejected(rejected)) => Output::Rejected(rejected.clone()),
        Some(Input::PermissionDenied(denied)) => Output::PermissionDenied(denied.clone()),
        Some(Input::ToolNotFound(not_found)) => Output::Error(pb::McpToolError {
            error: format!("MCP tool not found: {}", not_found.name),
            read_tool_def_reminder: String::new(),
        }),
        Some(Input::ServerNotFound(not_found)) => Output::Error(pb::McpToolError {
            error: format!("MCP server not found: {}", not_found.name),
            read_tool_def_reminder: String::new(),
        }),
        Some(Input::Approved(_)) | None => unreachable!("exec_output validated the MCP result"),
    };
    pb::McpToolResult {
        result: Some(result),
    }
}

fn task_result(result: &pb::SubagentResult) -> pb::TaskResult {
    use pb::{subagent_result::Result as Input, task_result::Result as Output};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => Output::Success(pb::TaskSuccess {
            agent_id: Some(success.agent_id.clone()),
            result_suffix: success.final_message.clone(),
            background_reason: success.background_reason,
            transcript_path: success.transcript_path.clone(),
            ..Default::default()
        }),
        Some(Input::Error(error)) => Output::Error(pb::TaskError {
            error: error.error.clone(),
        }),
        None => unreachable!("exec_output validated the subagent result"),
    };
    pb::TaskResult {
        result: Some(result),
    }
}

fn glob_result(result: &pb::GrepResult) -> pb::GlobToolResult {
    use pb::{glob_tool_result::Result as Output, grep_result::Result as Input};
    let result = match result.result.as_ref() {
        Some(Input::Success(success)) => {
            let files_result = success
                .active_editor_result
                .iter()
                .chain(success.workspace_results.values())
                .find_map(|result| match result.result.as_ref() {
                    Some(pb::grep_union_result::Result::Files(files)) => Some(files),
                    _ => None,
                });
            let (files, total_files, client_truncated, ripgrep_truncated) = files_result
                .map(|files| {
                    (
                        files.files.clone(),
                        files.total_files,
                        files.client_truncated,
                        files.ripgrep_truncated,
                    )
                })
                .unwrap_or_default();
            Output::Success(pb::GlobToolSuccess {
                pattern: success.pattern.clone(),
                path: success.path.clone(),
                files,
                total_files,
                client_truncated,
                ripgrep_truncated,
            })
        }
        Some(Input::Error(error)) => Output::Error(pb::GlobToolError {
            error: error.error.clone(),
        }),
        None => unreachable!("exec_output validated the glob result"),
    };
    pb::GlobToolResult {
        result: Some(result),
    }
}

pub(crate) fn todo_items(arguments: &Value) -> Vec<pb::TodoItem> {
    arguments
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
            status: match todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
            {
                "in_progress" => pb::TodoStatus::InProgress as i32,
                "completed" => pb::TodoStatus::Completed as i32,
                "cancelled" => pb::TodoStatus::Cancelled as i32,
                _ => pb::TodoStatus::Pending as i32,
            },
            created_at: 0,
            updated_at: 0,
            dependencies: todo
                .get("dependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn web_search_approval_is_not_a_terminal_tool_result() {
        let pending = PendingClientTool {
            call: ToolCall {
                index: 0,
                call_id: "call-1".into(),
                model_call_id: "model-1".into(),
                name: "WebSearch".into(),
                arguments_text: r#"{"search_term":"rust"}"#.into(),
                arguments: json!({"search_term": "rust"}),
            },
            context: crate::cursor::pending::ExecContext::default(),
            started_at_ms: 1,
        };
        let response = pb::InteractionResponse {
            id: 1,
            result: Some(pb::interaction_response::Result::WebSearchRequestResponse(
                pb::WebSearchRequestResponse {
                    result: Some(pb::web_search_request_response::Result::Approved(
                        pb::web_search_request_response::Approved {},
                    )),
                },
            )),
        };
        let error = from_interaction(pending, &response).unwrap_err();
        assert!(error
            .to_string()
            .contains("approval is not a terminal tool result"));
    }
}
