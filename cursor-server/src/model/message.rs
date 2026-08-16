use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Prompt,
    User,
    Runtime,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCallContent {
    pub index: usize,
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResultContent {
    pub call_id: String,
    pub name: String,
    pub output: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageContent {
    Text {
        text: String,
    },
    Assistant {
        text: String,
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_call_id: Option<String>,
        tool_calls: Vec<ToolCallContent>,
    },
    ToolResult(ToolResultContent),
    Json {
        value: Value,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CanonicalMessage {
    pub message_id: String,
    pub role: Role,
    pub origin: Origin,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_event_id: Option<String>,
}

impl CanonicalMessage {
    pub fn text(
        message_id: impl Into<String>,
        role: Role,
        origin: Origin,
        text: impl Into<String>,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            role,
            origin,
            content: MessageContent::Text { text: text.into() },
            runtime_event_id: None,
        }
    }
}
