use serde_json::Value;

use crate::{cursor::proto::agent::v1 as pb, model::ToolResult, Error, Result};

use super::{prost_json, ToolCompletion};
use crate::cursor::tools::runtime::PendingExec;

pub(super) fn complete(
    pending: PendingExec,
    result: &pb::McpStateExecResult,
) -> Result<ToolCompletion> {
    let call = &pending.call;
    let server_filter = call.arguments.get("server").and_then(Value::as_str);
    let tool_filter = call.arguments.get("toolName").and_then(Value::as_str);
    if tool_filter.is_some() && server_filter.is_none() {
        return Err(Error::Protocol(
            "GetMcpTools toolName requires server".into(),
        ));
    }
    let pattern = call
        .arguments
        .get("pattern")
        .and_then(Value::as_str)
        .map(regex::Regex::new)
        .transpose()
        .map_err(|error| Error::Protocol(format!("invalid GetMcpTools pattern: {error}")))?;
    let args = pb::GetMcpToolsArgs {
        server: server_filter.map(str::to_string),
        tool_name: tool_filter.map(str::to_string),
        pattern: call
            .arguments
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_string),
        tool_call_id: call.call_id.clone(),
    };
    let (content, is_error, result) = match result
        .result
        .as_ref()
        .ok_or_else(|| Error::Protocol("McpStateExecResult is missing result".into()))?
    {
        pb::mcp_state_exec_result::Result::Success(success) => {
            let matches = success
                .servers
                .iter()
                .filter(|server| {
                    server_filter.is_none_or(|value| value == server.server_identifier)
                })
                .flat_map(|server| {
                    server.tools.iter().filter_map(|tool| {
                        if tool_filter.is_some_and(|value| value != tool.tool_name)
                            || pattern.as_ref().is_some_and(|pattern| {
                                !pattern.is_match(&server.server_identifier)
                                    && !pattern.is_match(&tool.tool_name)
                            })
                        {
                            return None;
                        }
                        Some(serde_json::json!({
                            "server": server.server_identifier,
                            "toolName": tool.tool_name,
                            "description": tool.description,
                            "inputSchema": schema(tool),
                        }))
                    })
                })
                .collect::<Vec<_>>();
            let content = serde_json::to_string_pretty(&matches)?;
            let wire = pb::get_mcp_tools_agent_result::Result::Success(pb::GetMcpToolsSuccess {
                content: content.clone(),
                output_file_path: None,
            });
            (content, false, wire)
        }
        pb::mcp_state_exec_result::Result::Error(error) => failure(&error.error),
        pb::mcp_state_exec_result::Result::Rejected(rejected) => failure(&rejected.reason),
    };
    Ok(ToolCompletion::new(
        call,
        pending.started_at_ms,
        ToolResult {
            call_id: call.call_id.clone(),
            content,
            is_error,
        },
        pb::tool_call::Tool::GetMcpToolsToolCall(pb::GetMcpToolsToolCall {
            args: Some(args),
            result: Some(pb::GetMcpToolsAgentResult {
                result: Some(result),
            }),
        }),
    ))
}

fn failure(message: &str) -> (String, bool, pb::get_mcp_tools_agent_result::Result) {
    (
        message.into(),
        true,
        pb::get_mcp_tools_agent_result::Result::Error(pb::GetMcpToolsError {
            error: message.into(),
        }),
    )
}

fn schema(tool: &pb::McpToolDefinition) -> Value {
    let raw = tool.input_schema_json.clone().unwrap_or_else(|| {
        tool.input_schema
            .as_ref()
            .map(prost_json)
            .and_then(|value| serde_json::to_string(&value).ok())
            .unwrap_or_else(|| "{}".into())
    });
    serde_json::from_str(&raw).unwrap_or(Value::String(raw))
}
