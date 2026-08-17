//! Interaction query dispatch and approval continuation.

use crate::{
    cursor::{interaction, proto::agent::v1 as pb},
    model::ToolCall,
    Result,
};

use super::{normalized, InteractionContinuation, ToolStart};
use crate::cursor::tools::{
    codec, result,
    runtime::{CursorToolRuntime, ExecContext, PendingInteraction},
};

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    context: &ExecContext,
) -> Result<ToolStart> {
    let id = runtime.reserve_interaction(call, context).await?;
    Ok(ToolStart {
        messages: vec![interaction::tool_query(id, call)?],
        completion: None,
    })
}

pub(super) async fn resume(
    runtime: &CursorToolRuntime,
    pending: PendingInteraction,
    response: &pb::InteractionResponse,
) -> Result<InteractionContinuation> {
    if normalized(&pending.call.name) == "webfetch"
        && matches!(
            response.result.as_ref(),
            Some(pb::interaction_response::Result::WebFetchRequestResponse(
                pb::WebFetchRequestResponse {
                    result: Some(pb::web_fetch_request_response::Result::Approved(_)),
                }
            ))
        )
    {
        let id = runtime
            .reserve_exec(&pending.call, &pending.context)
            .await?;
        return Ok(InteractionContinuation::Message(Box::new(codec::request(
            id,
            &pending.call,
            &pending.context,
        )?)));
    }
    Ok(InteractionContinuation::Completed(Box::new(
        result::from_interaction(pending, response)?,
    )))
}
