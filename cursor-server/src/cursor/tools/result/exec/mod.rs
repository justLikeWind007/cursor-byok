mod output;
mod render;

use crate::{
    cursor::{interaction, proto::agent::v1 as pb},
    model::ToolResult,
    Error, Result,
};

use super::{mcp_state, ToolCompletion};
use crate::cursor::tools::{
    edit,
    runtime::{ExecStage, PendingExec},
};

pub(crate) fn from_exec(
    pending: PendingExec,
    wire_result: &pb::exec_client_message::Message,
) -> Result<ToolCompletion> {
    use pb::{exec_client_message::Message, tool_call::Tool};
    if let Message::McpStateExecResult(result) = wire_result {
        return mcp_state::complete(pending, result);
    }
    let call = &pending.call;
    let (content, is_error) = output::output(wire_result, call)?;
    let mut rendered = interaction::render_tool_call(call, false)?;
    match (rendered.tool.as_mut(), wire_result) {
        (Some(Tool::ShellToolCall(tool)), Message::ShellResult(result))
        | (Some(Tool::ShellToolCall(tool)), Message::MiniSweAgentBashResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::DeleteToolCall(tool)), Message::DeleteResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::GrepToolCall(tool)), Message::GrepResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::GlobToolCall(tool)), Message::GrepResult(result)) => {
            tool.result = Some(render::glob(result)?);
        }
        (Some(Tool::ReadToolCall(tool)), Message::ReadResult(result))
        | (Some(Tool::ReadToolCall(tool)), Message::RedactedReadResult(result)) => {
            tool.result = Some(render::read(result, call)?);
        }
        (Some(Tool::ReadLintsToolCall(tool)), Message::DiagnosticsResult(result)) => {
            tool.result = Some(render::diagnostics(result)?);
        }
        (Some(Tool::McpToolCall(tool)), Message::McpResult(result)) => {
            tool.result = Some(render::mcp(result)?);
        }
        (Some(Tool::ReadMcpResourceToolCall(tool)), Message::ReadMcpResourceExecResult(result)) => {
            tool.result = Some(result.clone());
        }
        (Some(Tool::WebFetchToolCall(tool)), Message::FetchResult(result)) => {
            tool.result = Some(render::web_fetch(result)?);
        }
        (Some(Tool::TaskToolCall(tool)), Message::SubagentResult(result)) => {
            tool.result = Some(render::task(result)?);
        }
        (Some(Tool::EditToolCall(tool)), Message::WriteResult(result)) => {
            tool.result = Some(match (&pending.stage, result.result.as_ref()) {
                (ExecStage::EditWrite(write), Some(pb::write_result::Result::Success(success))) => {
                    edit::success(success.path.clone(), write)
                }
                _ => render::write(result)?,
            });
        }
        _ => {
            return Err(Error::Protocol(format!(
                "unexpected Exec result for tool {}",
                call.name
            )));
        }
    }
    let tool = rendered.tool.ok_or_else(|| {
        Error::Protocol(format!("tool {} has no Cursor representation", call.name))
    })?;
    Ok(ToolCompletion::new(
        call,
        pending.started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            content,
            is_error,
        },
        tool,
    ))
}

pub(crate) fn edit_failure(pending: PendingExec, error: String) -> Result<ToolCompletion> {
    let call = &pending.call;
    let mut rendered = interaction::render_tool_call(call, false)?;
    let Some(pb::tool_call::Tool::EditToolCall(mut tool)) = rendered.tool.take() else {
        return Err(Error::Protocol(format!(
            "{} is not an edit tool",
            call.name
        )));
    };
    tool.result = Some(edit::failure(edit::path(call)?, error.clone()));
    Ok(ToolCompletion::new(
        call,
        pending.started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            content: error,
            is_error: true,
        },
        pb::tool_call::Tool::EditToolCall(tool),
    ))
}
