use std::collections::HashMap;

use serde_json::{json, Value};

use crate::{
    model::{CanonicalMessage, MessageContent, Role, ToolCallContent, ToolResultContent},
    Error, Result,
};

use super::ProviderMessage;

pub fn project_messages(messages: &[CanonicalMessage]) -> Result<Vec<ProviderMessage>> {
    let mut projected = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        if let Some((group, next_index)) = project_tool_group(messages, index)? {
            projected.extend(group);
            index = next_index;
        } else {
            projected.push(project_message(&messages[index])?);
            index += 1;
        }
    }
    Ok(projected)
}

fn project_tool_group(
    messages: &[CanonicalMessage],
    start: usize,
) -> Result<Option<(Vec<ProviderMessage>, usize)>> {
    let MessageContent::Assistant {
        model_call_id: Some(group_id),
        tool_calls,
        ..
    } = &messages[start].content
    else {
        return Ok(None);
    };
    if tool_calls.is_empty() {
        return Ok(None);
    }

    let mut cursor = start;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls = Vec::<ToolCallContent>::new();
    let mut results = HashMap::<String, ToolResultContent>::new();

    while cursor < messages.len() {
        let MessageContent::Assistant {
            text: part_text,
            thinking: part_thinking,
            model_call_id: Some(candidate_group),
            tool_calls: part_calls,
        } = &messages[cursor].content
        else {
            break;
        };
        if candidate_group != group_id || part_calls.is_empty() {
            break;
        }
        text.push_str(part_text);
        thinking.push_str(part_thinking);
        calls.extend(part_calls.iter().cloned());
        cursor += 1;

        while cursor < messages.len() {
            let MessageContent::ToolResult(result) = &messages[cursor].content else {
                break;
            };
            if !calls.iter().any(|call| call.call_id == result.call_id) {
                break;
            }
            if results
                .insert(result.call_id.clone(), result.clone())
                .is_some()
            {
                return Err(Error::Protocol(format!(
                    "duplicate tool result call_id: {}",
                    result.call_id
                )));
            }
            cursor += 1;
        }
    }

    calls.sort_by_key(|call| call.index);
    for call in &calls {
        if !results.contains_key(&call.call_id) {
            return Err(Error::Protocol(format!(
                "assistant tool call has no result call_id: {}",
                call.call_id
            )));
        }
    }

    let mut output = Vec::with_capacity(calls.len() + 1);
    output.push(ProviderMessage {
        role: "assistant".into(),
        content: Value::String(text),
        thinking: (!thinking.is_empty()).then_some(thinking),
        tool_call_id: None,
        tool_calls: Some(
            calls
                .iter()
                .map(project_tool_call)
                .collect::<Result<Vec<_>>>()?,
        ),
    });
    for call in &calls {
        output.push(project_tool_result(&results[&call.call_id])?);
    }
    Ok(Some((output, cursor)))
}

fn project_message(message: &CanonicalMessage) -> Result<ProviderMessage> {
    match &message.content {
        MessageContent::Assistant {
            text,
            thinking,
            tool_calls,
            ..
        } => Ok(ProviderMessage {
            role: "assistant".into(),
            content: Value::String(text.clone()),
            thinking: (!thinking.is_empty()).then(|| thinking.clone()),
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(
                    tool_calls
                        .iter()
                        .map(project_tool_call)
                        .collect::<Result<Vec<_>>>()?,
                )
            },
        }),
        MessageContent::ToolResult(result) => project_tool_result(result),
        MessageContent::Text { text } => Ok(ProviderMessage {
            role: role_name(&message.role).into(),
            content: Value::String(text.clone()),
            thinking: None,
            tool_call_id: None,
            tool_calls: None,
        }),
        MessageContent::Json { value } => Ok(ProviderMessage {
            role: role_name(&message.role).into(),
            content: value.clone(),
            thinking: None,
            tool_call_id: None,
            tool_calls: None,
        }),
    }
}

fn project_tool_call(call: &ToolCallContent) -> Result<Value> {
    Ok(json!({
        "id": call.call_id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": serde_json::to_string(&call.arguments)?
        }
    }))
}

fn project_tool_result(result: &ToolResultContent) -> Result<ProviderMessage> {
    let content = match result.output.as_str() {
        Some(text) => text.to_string(),
        None => serde_json::to_string(&result.output)?,
    };
    Ok(ProviderMessage {
        role: "tool".into(),
        content: Value::String(content),
        thinking: None,
        tool_call_id: Some(result.call_id.clone()),
        tool_calls: None,
    })
}

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}
