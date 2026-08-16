use prost::Message;

use crate::{
    cursor::proto::{agent::v1 as agent, aiserver::v1 as ai},
    run::{RunCommand, RunRegistry},
    Error, Result,
};

pub async fn append(
    registry: &RunRegistry,
    request: ai::BidiAppendRequest,
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
    if let Some(agent::agent_client_message::Message::RunRequest(run)) = &message.message {
        if let Some(conversation_id) = run.conversation_id.as_deref() {
            registry
                .bind_conversation(conversation_id, request_id)
                .await;
        }
    }
    registry
        .get_or_create(request_id)
        .await?
        .command(RunCommand::Append {
            seqno: request.append_seqno,
            message: Box::new(message),
        })
        .await?;
    Ok(ai::BidiAppendResponse {})
}
