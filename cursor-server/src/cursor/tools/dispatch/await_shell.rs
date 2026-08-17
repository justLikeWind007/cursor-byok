//! AwaitShell's timed and file-backed execution paths.

use crate::{model::ToolCall, Error, Result};

use super::ToolStart;
use crate::cursor::tools::{
    codec, result,
    result::ToolResultSender,
    runtime::{CursorToolRuntime, ExecContext},
};

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    results: &ToolResultSender,
    call: &ToolCall,
    context: &ExecContext,
) -> Result<ToolStart> {
    let message = if call
        .arguments
        .get("shell_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
    {
        let id = runtime.reserve_await(call, context).await?;
        Some(codec::await_read_request(id, call, context)?)
    } else {
        wait_without_shell_id(results, call)?;
        None
    };
    Ok(ToolStart {
        messages: message.into_iter().collect(),
        completion: None,
    })
}

fn wait_without_shell_id(results: &ToolResultSender, call: &ToolCall) -> Result<()> {
    let block_ms = call
        .arguments
        .get("block_until_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(30_000);
    if block_ms == 0 || block_ms > 7_140_000 {
        return Err(Error::Protocol(
            "AwaitShell without shell_id requires block_until_ms in 1..=7140000".into(),
        ));
    }
    let call = call.clone();
    let results = results.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(block_ms)).await;
        results.send(result::await_sleep(&call, block_ms));
    });
    Ok(())
}
