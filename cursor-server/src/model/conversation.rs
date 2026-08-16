use serde::{Deserialize, Serialize};

use super::Usage;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Conversation {
    pub conversation_id: String,
    pub revision: i64,
    pub head_blob_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Turn {
    pub request_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub revision: i64,
    pub status: TurnStatus,
    pub usage: Usage,
}
