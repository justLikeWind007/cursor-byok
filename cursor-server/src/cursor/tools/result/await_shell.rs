use serde_json::Value;

use crate::{
    cursor::proto::agent::v1 as pb,
    model::{ToolCall, ToolResult},
    Error, Result,
};

use super::{now_ms, ToolCompletion};
use crate::cursor::tools::runtime::{ExecStage, PendingExec};

pub(crate) fn await_result(
    pending: PendingExec,
    output_length: u64,
    regex_match: Option<String>,
    exit_code: Option<i32>,
) -> Result<ToolCompletion> {
    let ExecStage::Await(state) = &pending.stage else {
        return Err(Error::Protocol(
            "AwaitShell completion reached a non-await execution stage".into(),
        ));
    };
    let runtime_ms = now_ms().saturating_sub(pending.started_at_ms);
    let result = if exit_code.is_some() {
        pb::await_success::AwaitResult::Complete(pb::AwaitTaskComplete {
            task_id: state.task_id.clone(),
            runtime_ms,
            output_file_path: state.output_file_path.clone(),
            output_length,
            regex_requested: state.regex.is_some(),
            regex_match,
            exit_code,
            wake_reason: Some("task_complete".into()),
        })
    } else {
        pb::await_success::AwaitResult::StillRunning(pb::AwaitTaskStillRunning {
            task_id: state.task_id.clone(),
            runtime_ms,
            output_file_path: state.output_file_path.clone(),
            output_length,
            regex_requested: state.regex.is_some(),
            regex_match,
            wake_reason: Some("timeout_or_pattern".into()),
        })
    };
    let content = serde_json::json!({
        "task_id": state.task_id,
        "output_file_path": state.output_file_path,
        "output_length": output_length,
        "exit_code": exit_code,
    })
    .to_string();
    completion(
        &pending,
        content,
        false,
        pb::await_result::Result::Success(pb::AwaitSuccess {
            await_result: Some(result),
        }),
    )
}

pub(crate) fn await_error(pending: PendingExec, error: &str) -> Result<ToolCompletion> {
    completion(
        &pending,
        error.into(),
        true,
        pb::await_result::Result::Error(pb::AwaitError {
            error: error.into(),
        }),
    )
}

fn completion(
    pending: &PendingExec,
    content: String,
    is_error: bool,
    result: pb::await_result::Result,
) -> Result<ToolCompletion> {
    let ExecStage::Await(state) = &pending.stage else {
        return Err(Error::Protocol(
            "AwaitShell completion reached a non-await execution stage".into(),
        ));
    };
    Ok(ToolCompletion::new(
        &pending.call,
        pending.started_at_ms,
        ToolResult {
            call_id: pending.call.call_id.clone(),
            content,
            is_error,
        },
        pb::tool_call::Tool::AwaitToolCall(pb::AwaitToolCall {
            args: Some(pb::AwaitArgs {
                task_id: state.task_id.clone(),
                block_until_ms: pending
                    .call
                    .arguments
                    .get("block_until_ms")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                regex: state.regex.clone(),
            }),
            result: Some(pb::AwaitResult {
                result: Some(result),
            }),
        }),
    ))
}

pub(crate) fn await_sleep(call: &ToolCall, runtime_ms: u64) -> ToolCompletion {
    ToolCompletion::new(
        call,
        now_ms().saturating_sub(runtime_ms),
        ToolResult {
            call_id: call.call_id.clone(),
            content: format!("Waited {runtime_ms} ms"),
            is_error: false,
        },
        pb::tool_call::Tool::AwaitToolCall(pb::AwaitToolCall {
            args: Some(pb::AwaitArgs {
                task_id: String::new(),
                block_until_ms: Some(runtime_ms as u32),
                regex: None,
            }),
            result: Some(pb::AwaitResult {
                result: Some(pb::await_result::Result::Success(pb::AwaitSuccess {
                    await_result: Some(pb::await_success::AwaitResult::StillRunning(
                        pb::AwaitTaskStillRunning {
                            task_id: String::new(),
                            runtime_ms,
                            output_file_path: String::new(),
                            output_length: 0,
                            regex_requested: false,
                            regex_match: None,
                            wake_reason: Some("sleep_complete".into()),
                        },
                    )),
                })),
            }),
        }),
    )
}
