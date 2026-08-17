use prost::Message;

use crate::{
    cursor::proto::{agent::v1 as agent, aiserver::v1 as ai},
    cursor::{CursorCommand, CursorParent, CursorSessionRegistry},
    Error, Result,
};

pub async fn append(
    registry: &CursorSessionRegistry,
    request: ai::BidiAppendRequest,
    parent: Option<CursorParent>,
) -> Result<ai::BidiAppendResponse> {
    let request_id = request
        .request_id
        .as_ref()
        .map(|id| id.request_id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Protocol("BidiAppend request_id is required".into()))?;
    if !request.data_binary.is_empty() {
        return Err(Error::Protocol(
            "BidiAppend data_binary is not part of the captured protocol".into(),
        ));
    }
    if request.data.is_empty() {
        return Err(Error::Protocol(
            "BidiAppend contains no AgentClientMessage".into(),
        ));
    }
    let payload = hex::decode(&request.data)
        .map_err(|error| Error::Protocol(format!("invalid BidiAppend hex: {error}")))?;
    let message = agent::AgentClientMessage::decode(payload.as_slice())?;
    let handle = registry.get_or_create(request_id).await?;
    if let Some(parent) = parent {
        handle.set_parent(parent)?;
    }
    handle
        .command(CursorCommand::Append {
            seqno: request.append_seqno,
            message: Box::new(message),
        })
        .await?;
    Ok(ai::BidiAppendResponse {})
}
