use serde_json::Value;

use crate::{
    cursor::{interaction, proto::agent::v1 as pb},
    model::{ToolCall, ToolResult},
    Error, Result,
};

use super::{now_ms, ToolCompletion};

pub(crate) fn local(call: &ToolCall, message_index: usize) -> Result<ToolCompletion> {
    match normalized(&call.name).as_str() {
        "todowrite" => todo_write(call),
        "updatecurrentstep" => update_current_step(call, message_index),
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
            content: call.arguments.to_string(),
            is_error: false,
        },
        tool,
    ))
}

fn update_current_step(call: &ToolCall, message_index: usize) -> Result<ToolCompletion> {
    let current_step = call
        .arguments
        .get("current_step")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut rendered = interaction::render_tool_call(call, false)?;
    let Some(pb::tool_call::Tool::CommunicateUpdateToolCall(tool)) = rendered.tool.as_mut() else {
        return Err(Error::Protocol(
            "UpdateCurrentStep has no Cursor representation".into(),
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
        .ok_or_else(|| Error::Protocol("UpdateCurrentStep has no Cursor representation".into()))?;
    Ok(ToolCompletion::new(
        call,
        now_ms(),
        ToolResult {
            call_id: call.call_id.clone(),
            content: serde_json::json!({
                "success": {
                    "current_step": current_step,
                    "message_index": message_index,
                }
            })
            .to_string(),
            is_error: false,
        },
        tool,
    ))
}

pub(crate) fn todo_items(arguments: &Value) -> Vec<pb::TodoItem> {
    arguments
        .get("todos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|todo| pb::TodoItem {
            id: text(todo, "id"),
            content: text(todo, "content"),
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

fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
