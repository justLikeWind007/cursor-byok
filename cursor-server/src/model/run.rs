use serde::{Deserialize, Serialize};

use super::{
    CanonicalMessage, ConversationId, ModelSpec, PromptSpec, RevisionId, RunId, ToolCall,
    ToolRoundAssistant,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubagentKind {
    GeneralPurpose,
    Named(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunKind {
    Root,
    Subagent {
        parent_run_id: RunId,
        parent_tool_call_id: String,
        kind: SubagentKind,
        background: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SubagentModelOverride {
    Explicit(ModelSpec),
    Inherit,
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum RunAction {
    Start,
    Resume {
        pending_tool_round: Option<RecoveredToolRound>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RecoveredToolRound {
    pub assistant: ToolRoundAssistant,
    pub calls: Vec<ToolCall>,
    pub started_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PreparedRun {
    pub run_id: RunId,
    pub conversation_id: ConversationId,
    pub kind: RunKind,
    pub model: ModelSpec,
    pub prompt: PromptSpec,
    pub selected_subagent_models: Vec<ModelSpec>,
    pub subagent_model_overrides: Vec<(SubagentKind, SubagentModelOverride)>,
    pub initial_messages: Vec<CanonicalMessage>,
    pub action: RunAction,
    pub base_revision_id: RevisionId,
}
