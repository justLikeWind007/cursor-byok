mod await_shell;
mod exec;
mod interaction;
mod local;
mod mcp_state;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    cursor::proto::agent::v1 as pb,
    model::{ToolCall, ToolResult},
    Error, Result,
};

use super::runtime::now_ms;

pub(crate) use await_shell::{await_error, await_result, await_sleep};
pub(crate) use exec::{edit_failure, from_exec};
pub(crate) use interaction::from_interaction;
pub(crate) use local::{local, todo_items};

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

    pub(super) fn new(
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

    pub(super) fn from_rendered(
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
                content: output,
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

pub(super) fn prost_json(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match value.kind.as_ref() {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(value)) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(Kind::StringValue(value)) => Value::String(value.clone()),
        Some(Kind::BoolValue(value)) => Value::Bool(*value),
        Some(Kind::StructValue(value)) => Value::Object(
            value
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), prost_json(value)))
                .collect(),
        ),
        Some(Kind::ListValue(value)) => Value::Array(value.values.iter().map(prost_json).collect()),
    }
}
