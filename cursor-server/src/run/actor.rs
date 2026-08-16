use std::sync::Arc;

use tokio::sync::mpsc;

use crate::{
    cursor::{
        blob_sync::BlobSynchronizer,
        exec,
        pending::{PendingClientTools, PendingExecRegistry},
        proto::agent::v1 as pb,
        tool_result::tool_result_channel,
        tools::{ClientToolEvent, ToolDispatcher},
    },
    model::Usage,
    prompting::PromptCompiler,
    provider::Provider,
    store::{RunStatus, Store},
};

use super::{lifecycle, LoopEngine, OrderedInbox, RunCommand, RunHandle};

pub struct RunActor;

#[derive(Clone)]
pub(crate) struct RunDependencies {
    pub store: Store,
    pub provider: Arc<dyn Provider>,
    pub compiler: PromptCompiler,
    pub model: String,
}

impl RunActor {
    pub(crate) fn spawn(
        handle: RunHandle,
        mut receiver: mpsc::Receiver<RunCommand>,
        dependencies: RunDependencies,
        blob_sync: BlobSynchronizer,
        next_append_seqno: i64,
    ) {
        tokio::spawn(async move {
            let store = dependencies.store.clone();
            let mut inbox = OrderedInbox::starting_at(next_append_seqno);
            let (results_tx, results_rx) = tool_result_channel();
            let pending_tools = PendingClientTools::default();
            let pending_execs = PendingExecRegistry::default();
            let tools = ToolDispatcher::new(pending_execs.clone(), pending_tools);
            let mut engine = Some(LoopEngine::new(
                handle.clone(),
                dependencies,
                blob_sync.clone(),
                results_rx,
                tools.clone(),
                pending_execs.clone(),
            ));
            let cancellation = handle.cancellation();
            loop {
                let command = tokio::select! {
                    _ = cancellation.cancelled() => {
                        let _ = store
                            .update_run_status(
                                handle.request_id(),
                                RunStatus::Interrupted,
                                Usage::default(),
                            )
                            .await;
                        for id in pending_execs.drain_running().await {
                            let _ = handle.emit(&exec::abort(id));
                        }
                        let _ = lifecycle::cancel(&handle);
                        break;
                    }
                    command = receiver.recv() => {
                        let Some(command) = command else { break };
                        command
                    }
                };
                match command {
                    RunCommand::Abort => {
                        handle.cancel();
                    }
                    RunCommand::Finished => {
                        break;
                    }
                    RunCommand::Append { seqno, message } => {
                        for (seqno, message) in inbox.push(seqno, *message) {
                            if store
                                .advance_append_seqno(handle.request_id(), seqno)
                                .await
                                .unwrap_or(false)
                            {
                                match message.message {
                                    Some(pb::agent_client_message::Message::RunRequest(
                                        request,
                                    )) => {
                                        if let Some(engine) = engine.take() {
                                            tokio::spawn(engine.run(request));
                                        }
                                    }
                                    Some(pb::agent_client_message::Message::ExecClientMessage(
                                        message,
                                    )) => {
                                        match exec::client_event(&message, &pending_execs).await {
                                            Ok(exec::ClientExecEvent::Delta(message)) => {
                                                let _ = handle.emit(&message);
                                            }
                                            Ok(exec::ClientExecEvent::Completed(result)) => {
                                                results_tx.send(*result)
                                            }
                                            Ok(exec::ClientExecEvent::Pending) => {}
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
                                                if pending_execs.take(close.id).await.is_some() {
                                                    results_tx.send_error(crate::Error::Protocol(format!(
                                                        "Exec stream closed before result for id: {}",
                                                        close.id
                                                    )));
                                                }
                                            }
                                            Some(Message::Throw(throw)) => {
                                                match pending_execs.take(throw.id).await {
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
