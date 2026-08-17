use std::time::Duration;

use tokio::sync::oneshot;

use crate::model::{RevisionId, ToolCall, ToolRoundId, Usage};
use crate::run::RunOutcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitCause {
    InitialMessages,
    ToolRoundStarted(ToolRoundId),
    ToolResult { call_id: String },
    FinalTurn,
    RuntimeEvent { event_id: String },
}

#[derive(Debug)]
pub enum CommitBarrier {
    None,
    BeforeContinue(oneshot::Sender<std::result::Result<(), String>>),
}

impl CommitBarrier {
    pub fn before_continue() -> (Self, oneshot::Receiver<std::result::Result<(), String>>) {
        let (sender, receiver) = oneshot::channel();
        (Self::BeforeContinue(sender), receiver)
    }

    pub fn is_required(&self) -> bool {
        matches!(self, Self::BeforeContinue(_))
    }

    pub fn complete(self, result: std::result::Result<(), String>) {
        if let Self::BeforeContinue(sender) = self {
            let _ = sender.send(result);
        }
    }
}

#[derive(Debug)]
pub struct StateCommitted {
    pub revision_id: RevisionId,
    pub tool_round_version: u64,
    pub cause: CommitCause,
    pub barrier: CommitBarrier,
}

#[derive(Debug)]
pub enum ClientEvent {
    TextStart,
    TextDelta(String),
    TextEnd,
    ThinkingStart,
    ThinkingDelta(String),
    ThinkingEnd {
        duration: Duration,
    },
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
        model_call_id: String,
    },
    ToolCallArgumentsDelta {
        index: usize,
        delta: String,
    },
    ToolCallEnd {
        index: usize,
    },
    Usage(Usage),
    ExecuteToolRound {
        round_id: ToolRoundId,
        calls: Vec<ToolCall>,
    },
    StateCommitted(StateCommitted),
    Ended(RunOutcome),
}
