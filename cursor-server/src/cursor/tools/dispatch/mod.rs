mod await_shell;
mod edit;
mod exec;
mod interaction;
mod local;

use std::collections::BTreeMap;

use crate::{cursor::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::{
    result::{ToolCompletion, ToolResultSender},
    runtime::{CursorToolRuntime, ExecContext, PendingInteraction},
};

pub(super) struct ToolStart {
    pub messages: Vec<pb::AgentServerMessage>,
    pub completion: Option<ToolCompletion>,
}

pub(super) enum InteractionContinuation {
    Message(Box<pb::AgentServerMessage>),
    Completed(Box<ToolCompletion>),
}

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    results: &ToolResultSender,
    call: &ToolCall,
    message_index: usize,
    dynamic_mcp: &BTreeMap<String, pb::McpToolDefinition>,
    context: &ExecContext,
) -> Result<ToolStart> {
    if let Some(definition) = dynamic_mcp.get(&call.name) {
        return exec::start_dynamic(runtime, call, definition, context).await;
    }

    match normalized(&call.name).as_str() {
        "shell" | "read" | "delete" | "grep" | "glob" | "readlints" | "task" | "callmcptool"
        | "fetchmcpresource" | "getmcptools" => exec::start(runtime, call, context).await,
        "write" | "strreplace" | "editnotebook" => edit::start(runtime, call, context).await,
        "askquestion" | "websearch" | "webfetch" | "switchmode" | "createplan"
        | "generateimage" => interaction::start(runtime, call, context).await,
        "todowrite" | "updatecurrentstep" => local::start(call, message_index),
        "awaitshell" => await_shell::start(runtime, results, call, context).await,
        _ => Err(Error::Protocol(format!("unsupported tool: {}", call.name))),
    }
}

pub(super) async fn resume_interaction(
    runtime: &CursorToolRuntime,
    pending: PendingInteraction,
    response: &pb::InteractionResponse,
) -> Result<InteractionContinuation> {
    interaction::resume(runtime, pending, response).await
}

pub(super) fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
