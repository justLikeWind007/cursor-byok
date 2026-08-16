use crate::{
    cursor::{
        checkpoint::CheckpointBuilder,
        connect::{
            encode_end_stream, encode_error_end_stream, ConnectCode, ConnectErrorDetail,
            ConnectStreamError,
        },
        proto::{agent::v1 as pb, aiserver::v1 as ai},
    },
    model::Usage,
    run::RunHandle,
    Error, Result,
};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use prost::Message;

pub async fn publish_success(
    handle: &RunHandle,
    usage: Usage,
    checkpoint_builder: &CheckpointBuilder,
    checkpoint: Option<&pb::ConversationStateStructure>,
) -> Result<()> {
    handle.emit(&crate::cursor::interaction::turn_ended(usage))?;
    if let Some(checkpoint) = checkpoint {
        checkpoint_builder.publish(handle, checkpoint).await?;
        let message = pb::AgentServerMessage {
            ttft_breakdown: None,
            message: Some(
                pb::agent_server_message::Message::ConversationCheckpointUpdate(checkpoint.clone()),
            ),
        };
        handle.emit(&message)?;
    }
    Ok(())
}

pub fn finish_success(handle: &RunHandle) {
    handle.emit_frame(encode_end_stream());
    handle.close_output();
}

pub fn fail(handle: &RunHandle, error: &Error) -> Result<()> {
    let stream_error = match error {
        Error::Provider(_) | Error::Http(_) => provider_error(error),
        Error::Protocol(_) | Error::Decode(_) | Error::Json(_) => {
            plain_error(ConnectCode::InvalidArgument, error)
        }
        Error::RunNotFound(_) => plain_error(ConnectCode::NotFound, error),
        Error::Cancelled => plain_error(ConnectCode::Canceled, error),
        Error::Config(_)
        | Error::Database(_)
        | Error::Migration(_)
        | Error::Encode(_)
        | Error::Io(_) => plain_error(ConnectCode::Internal, error),
    };
    let frame = encode_error_end_stream(&stream_error)?;
    handle.emit_frame(frame);
    handle.close_output();
    Ok(())
}

pub fn cancel(handle: &RunHandle) -> Result<()> {
    let frame = encode_error_end_stream(&ConnectStreamError {
        code: ConnectCode::Canceled,
        message: "run was cancelled".into(),
        details: Vec::new(),
    })?;
    handle.emit_frame(frame);
    handle.close_output();
    Ok(())
}

fn plain_error(code: ConnectCode, error: &Error) -> ConnectStreamError {
    ConnectStreamError {
        code,
        message: error.to_string(),
        details: Vec::new(),
    }
}

fn provider_error(error: &Error) -> ConnectStreamError {
    let detail = ai::ErrorDetails {
        error: ai::error_details::Error::ProviderError as i32,
        details: Some(ai::CustomErrorDetails {
            title: "Server Error".into(),
            detail: error.to_string(),
            allow_command_links_potentially_unsafe_please_only_use_for_handwritten_trusted_markdown:
                Some(true),
            is_retryable: Some(true),
            show_request_id: Some(true),
            should_show_immediate_error: Some(false),
        }),
        is_expected: Some(false),
    };
    ConnectStreamError {
        code: ConnectCode::Unavailable,
        message: error.to_string(),
        details: vec![ConnectErrorDetail {
            type_name: "aiserver.v1.ErrorDetails".into(),
            value: STANDARD_NO_PAD.encode(detail.encode_to_vec()),
        }],
    }
}
