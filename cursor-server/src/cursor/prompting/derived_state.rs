use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{CanonicalMessage, MessageContent};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DerivedState {
    pub todos: Option<Value>,
    pub plan: Option<Value>,
}

pub fn fold_derived_state(messages: &[CanonicalMessage]) -> DerivedState {
    let mut state = DerivedState::default();
    let mut calls = std::collections::HashMap::<String, (String, Value)>::new();
    for message in messages {
        match &message.content {
            MessageContent::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    calls.insert(
                        call.call_id.clone(),
                        (call.name.clone(), call.arguments.clone()),
                    );
                }
            }
            MessageContent::ToolResult(result) if !result.is_error => {
                let Some((name, input)) = calls.get(&result.call_id).cloned() else {
                    continue;
                };
                match normalize(&name).as_str() {
                    "todowrite" | "updatetodos" => state.todos = Some(input),
                    "createplan" | "updateplan" | "writeplan" => state.plan = Some(input),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    state
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
