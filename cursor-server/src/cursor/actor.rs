use std::sync::Arc;

use tokio::sync::mpsc;

use crate::{
    cursor::prompting::PromptCompiler,
    cursor::{
        blob_sync::BlobSynchronizer,
        checkpoint::CheckpointBuilder,
        proto::agent::v1 as pb,
        request,
        session::CursorSession,
        tools::{
            codec, result::tool_result_channel, runtime::CursorToolRuntime, ClientToolEvent,
            ToolDispatcher,
        },
    },
    provider::Provider,
    run::{RunActor, RunRegistry},
    store::Store,
};

use super::{inbox::OrderedInbox, CursorCommand, CursorSessionHandle};

pub struct CursorActor;

#[derive(Clone)]
pub(crate) struct RunDependencies {
    pub store: Store,
    pub provider: Arc<dyn Provider>,
    pub compiler: PromptCompiler,
    pub run_registry: RunRegistry,
}

impl CursorActor {
    pub(crate) fn spawn(
        handle: CursorSessionHandle,
        mut receiver: mpsc::Receiver<CursorCommand>,
        dependencies: RunDependencies,
        blob_sync: BlobSynchronizer,
        next_append_seqno: i64,
    ) {
        tokio::spawn(async move {
            let mut inbox = OrderedInbox::starting_at(next_append_seqno);
            let (results_tx, results_rx) = tool_result_channel();
            let tool_runtime = CursorToolRuntime::default();
            let tools = ToolDispatcher::with_results(tool_runtime.clone(), results_tx.clone());
            let mut run_resources = Some((results_rx, dependencies));
            loop {
                let command = match receiver.recv().await {
                    Some(command) => command,
                    None => {
                        handle.cancel();
                        break;
                    }
                };
                match command {
                    CursorCommand::Abort => {
                        handle.cancel();
                    }
                    CursorCommand::Finished => {
                        break;
                    }
                    CursorCommand::Append { seqno, message } => {
                        for (_seqno, message) in inbox.push(seqno, *message) {
                            {
                                match message.message {
                                    Some(pb::agent_client_message::Message::RunRequest(
                                        request,
                                    )) => {
                                        if let Some((results, dependencies)) = run_resources.take()
                                        {
                                            let handle = handle.clone();
                                            let blob_sync = blob_sync.clone();
                                            let tools = tools.clone();
                                            let tool_runtime = tool_runtime.clone();
                                            tokio::spawn(async move {
                                                let mut checkpoint = CheckpointBuilder::new(
                                                    dependencies.store.clone(),
                                                    blob_sync.clone(),
                                                    handle
                                                        .parent()
                                                        .map(|parent| parent.tool_call_id.clone()),
                                                    request.conversation_state.clone(),
                                                );
                                                let parent = handle.parent().map(|parent| {
                                                    (
                                                        crate::model::RunId::new(&parent.run_id),
                                                        parent.tool_call_id.clone(),
                                                    )
                                                });
                                                let prepared = request::prepare(
                                                    handle.request_id(),
                                                    &request,
                                                    parent,
                                                    request::PrepareDependencies {
                                                        compiler: &dependencies.compiler,
                                                        store: &dependencies.store,
                                                        checkpoint: &checkpoint,
                                                        blob_sync: &blob_sync,
                                                    },
                                                )
                                                .await;
                                                let (prepared, context) = match prepared {
                                                    Ok(prepared) => prepared,
                                                    Err(error) => {
                                                        tracing::error!(
                                                            request_id = handle.request_id(),
                                                            %error,
                                                            "failed to prepare Cursor Run"
                                                        );
                                                        let _ = crate::cursor::lifecycle::fail(
                                                            &handle, &error,
                                                        );
                                                        let _ = handle
                                                            .command(CursorCommand::Finished)
                                                            .await;
                                                        return;
                                                    }
                                                };
                                                checkpoint.configure(
                                                    prepared.model.model_id.clone(),
                                                    prepared.model.context_window_tokens,
                                                    prepared.prompt.instructions.clone(),
                                                    prepared.prompt.tools.clone(),
                                                    context.dynamic_tools.keys().cloned().collect(),
                                                    context.turn_user.clone(),
                                                );
                                                let cancellation = handle.cancellation();
                                                let (port, core) = crate::client::session(256);
                                                let actor = RunActor::new(
                                                    dependencies.store.clone(),
                                                    dependencies.provider,
                                                    dependencies.run_registry,
                                                );
                                                let core_run =
                                                    actor.spawn(prepared, port, cancellation).await;
                                                let session = CursorSession::new(
                                                    handle.clone(),
                                                    dependencies.store,
                                                    context,
                                                    core,
                                                    super::session::CursorSessionRuntime {
                                                        tools,
                                                        results,
                                                        checkpoint,
                                                        tool_runtime,
                                                    },
                                                );
                                                if let Err(error) = session.run().await {
                                                    tracing::error!(
                                                        request_id = handle.request_id(),
                                                        %error,
                                                        "Cursor session failed"
                                                    );
                                                    handle.cancel();
                                                    let _ = crate::cursor::lifecycle::fail(
                                                        &handle, &error,
                                                    );
                                                }
                                                let _ = core_run.await;
                                                let _ =
                                                    handle.command(CursorCommand::Finished).await;
                                            });
                                        }
                                    }
                                    Some(pb::agent_client_message::Message::ExecClientMessage(
                                        message,
                                    )) => {
                                        match codec::client_event(&message, &tool_runtime).await {
                                            Ok(codec::ClientExecEvent::Delta(message)) => {
                                                let _ = handle.emit(&message);
                                            }
                                            Ok(codec::ClientExecEvent::Message(message)) => {
                                                let _ = handle.emit(&message);
                                            }
                                            Ok(codec::ClientExecEvent::Completed(result)) => {
                                                results_tx.send(*result)
                                            }
                                            Ok(codec::ClientExecEvent::Pending) => {}
                                            Err(error) => results_tx.send_error(error),
                                        }
                                    }
                                    Some(
                                        pb::agent_client_message::Message::ExecClientControlMessage(
                                            message,
                                        ),
                                    ) => {
                                        use pb::exec_client_control_message::Message;
                                        match message.message {
                                            Some(Message::StreamClose(close)) => {
                                                if tool_runtime.take_exec(close.id).await.is_some()
                                                {
                                                    results_tx.send_error(crate::Error::Protocol(format!(
                                                        "Exec stream closed before result for id: {}",
                                                        close.id
                                                    )));
                                                }
                                            }
                                            Some(Message::Throw(throw)) => {
                                                match tool_runtime.take_exec(throw.id).await {
                                                    Some(pending) => results_tx.send_error(
                                                        crate::Error::Protocol(format!(
                                                            "Exec {} failed: {}",
                                                            pending.call.call_id, throw.error
                                                        )),
                                                    ),
                                                    None => results_tx.send_error(
                                                        crate::Error::Protocol(format!(
                                                            "unknown ExecClientThrow id: {}",
                                                            throw.id
                                                        )),
                                                    ),
                                                }
                                            }
                                            Some(Message::Heartbeat(_)) | None => {}
                                        }
                                    }
                                    Some(
                                        pb::agent_client_message::Message::InteractionResponse(
                                            message,
                                        ),
                                    ) => match tools.interaction_response(&message).await {
                                        Ok(ClientToolEvent::Message(message)) => {
                                            let _ = handle.emit(&message);
                                        }
                                        Ok(ClientToolEvent::Completed(completion)) => {
                                            results_tx.send(*completion)
                                        }
                                        Err(error) => results_tx.send_error(error),
                                    },
                                    Some(pb::agent_client_message::Message::KvClientMessage(
                                        message,
                                    )) => {
                                        let _ = blob_sync.handle_client(message).await;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}
