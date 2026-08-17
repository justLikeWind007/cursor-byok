use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use tokio::sync::oneshot;

use crate::{
    client::{ClientCommand, ClientEvent, ClientSession, CommitCause},
    cursor::{
        checkpoint::{
            worker::{CheckpointJob, CheckpointKind, CheckpointWorker, FinalCheckpoints},
            CheckpointBuilder,
        },
        interaction,
        presentation::Presentation,
        proto::agent::v1 as pb,
        request::CursorRunContext,
        tools::{
            codec,
            result::{ToolCompletion, ToolResultReceiver},
            runtime::CursorToolRuntime,
            stream::ToolCallStream,
            ToolBatchState, ToolDispatcher,
        },
    },
    model::{ToolCall, ToolRoundId, Usage},
    run::{RunFailure, RunOutcome},
    store::{Store, ToolRoundStatus},
    Error, Result,
};

use super::CursorSessionHandle;

pub struct CursorSession {
    handle: CursorSessionHandle,
    store: Store,
    context: CursorRunContext,
    core: ClientSession,
    tools: ToolDispatcher,
    results: ToolResultReceiver,
    checkpoint: CheckpointBuilder,
    tool_runtime: CursorToolRuntime,
}

pub(crate) struct CursorSessionRuntime {
    pub tools: ToolDispatcher,
    pub results: ToolResultReceiver,
    pub checkpoint: CheckpointBuilder,
    pub tool_runtime: CursorToolRuntime,
}

impl CursorSession {
    pub(crate) fn new(
        handle: CursorSessionHandle,
        store: Store,
        context: CursorRunContext,
        core: ClientSession,
        runtime: CursorSessionRuntime,
    ) -> Self {
        Self {
            handle,
            store,
            context,
            core,
            tools: runtime.tools,
            results: runtime.results,
            checkpoint: runtime.checkpoint,
            tool_runtime: runtime.tool_runtime,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let mut worker = CheckpointWorker::spawn(
            self.store.clone(),
            self.checkpoint.clone(),
            self.handle.clone(),
            self.context.mode,
        );
        let mut checkpoint_worker_open = true;
        let mut calls = BTreeMap::<usize, ToolCall>::new();
        let mut streams = BTreeMap::<usize, ToolCallStream>::new();
        let mut completions = HashMap::<String, ToolCompletion>::new();
        let mut completed = HashSet::<String>::new();
        let mut response_text = String::new();
        let mut response_thinking = String::new();
        let mut active_round = None::<ToolRoundId>;
        let mut final_checkpoint = None::<FinalCheckpoints>;
        let mut turn_usage = None::<Usage>;
        let mut context_tokens = None::<u64>;
        let mut ready = VecDeque::new();
        let mut presentation = Presentation::default();

        loop {
            let input = if let Some(completion) = ready.pop_front() {
                Input::Completion(completion)
            } else {
                tokio::select! {
                    event = self.core.events.recv() => Input::Event(event),
                    completion = self.results.recv() => Input::CompletionResult(completion),
                    failure = worker.failures.recv(), if checkpoint_worker_open => Input::CheckpointFailure(failure),
                }
            };
            match input {
                Input::CheckpointFailure(Some(error)) => return Err(error),
                Input::CheckpointFailure(None) => {
                    checkpoint_worker_open = false;
                }
                Input::Completion(completion) => {
                    self.forward_completion(completion, &mut completions)
                        .await?;
                }
                Input::CompletionResult(Some(result)) => {
                    self.forward_completion(result?, &mut completions).await?;
                }
                Input::CompletionResult(None) => {
                    return Err(Error::Protocol("tool result channel closed".into()));
                }
                Input::Event(None) => {
                    worker.abort();
                    return Err(Error::Protocol("core event channel closed".into()));
                }
                Input::Event(Some(event)) => match event {
                    ClientEvent::TextStart => {}
                    ClientEvent::TextEnd => presentation.finish_text(),
                    ClientEvent::TextDelta(delta) => {
                        response_text.push_str(&delta);
                        presentation.text_delta(&delta);
                        self.emit_model_event(crate::provider::ModelEvent::TextDelta(delta), "")?;
                    }
                    ClientEvent::ThinkingStart => {}
                    ClientEvent::ThinkingDelta(delta) => {
                        response_thinking.push_str(&delta);
                        presentation.thinking_delta(&delta);
                        self.emit_model_event(
                            crate::provider::ModelEvent::ThinkingDelta(delta),
                            "",
                        )?;
                    }
                    ClientEvent::ThinkingEnd { duration } => {
                        presentation.finish_thinking(duration);
                        self.handle
                            .emit(&interaction::thinking_completed(duration))?;
                    }
                    ClientEvent::ToolCallStart {
                        index,
                        call_id,
                        name,
                        model_call_id,
                    } => {
                        let call = ToolCall {
                            index,
                            call_id: call_id.clone(),
                            model_call_id: model_call_id.clone(),
                            name: name.clone(),
                            arguments_text: String::new(),
                            arguments: serde_json::Value::Null,
                        };
                        self.emit_model_event(
                            crate::provider::ModelEvent::ToolCallStart {
                                index,
                                call_id,
                                name: name.clone(),
                            },
                            &model_call_id,
                        )?;
                        streams.insert(index, ToolCallStream::new(&name));
                        calls.insert(index, call);
                    }
                    ClientEvent::ToolCallArgumentsDelta { index, delta } => {
                        let call = calls.get_mut(&index).ok_or_else(|| {
                            Error::Protocol(format!("unknown streaming tool index: {index}"))
                        })?;
                        call.arguments_text.push_str(&delta);
                        let stream = streams.get_mut(&index).ok_or_else(|| {
                            Error::Protocol(format!("missing Cursor tool stream: {index}"))
                        })?;
                        for message in stream.arguments_delta(call, &delta)? {
                            self.handle.emit(&message)?;
                        }
                    }
                    ClientEvent::ToolCallEnd { index } => {
                        let call = calls.get_mut(&index).ok_or_else(|| {
                            Error::Protocol(format!("unknown completed tool index: {index}"))
                        })?;
                        call.arguments = serde_json::from_str(&call.arguments_text)?;
                    }
                    ClientEvent::Usage(usage) => {
                        if let Some(output_tokens) = usage.output_tokens {
                            self.handle.emit(&interaction::token_delta(output_tokens))?;
                        }
                        context_tokens = usage
                            .input_tokens
                            .zip(usage.output_tokens)
                            .and_then(|(input, output)| input.checked_add(output));
                        match &mut turn_usage {
                            Some(total) => *total += usage,
                            None => turn_usage = Some(usage),
                        }
                    }
                    ClientEvent::ExecuteToolRound {
                        round_id,
                        calls: round_calls,
                    } => {
                        active_round = Some(round_id);
                        for dispatched in self
                            .tools
                            .start_batch(
                                &round_calls,
                                ToolBatchState {
                                    completed: &completed,
                                    started: &HashSet::new(),
                                    response_text: &response_text,
                                    response_thinking: &response_thinking,
                                },
                                &self
                                    .store
                                    .load_current_messages(&crate::model::ConversationId::new(
                                        &self.context.exec.conversation_id,
                                    ))
                                    .await?,
                                &self.context.dynamic_tools,
                                &self.context.exec,
                            )
                            .await?
                        {
                            for message in dispatched.messages {
                                self.handle.emit(&message)?;
                            }
                            if let Some(completion) = dispatched.completion {
                                ready.push_back(completion);
                            }
                        }
                        response_text.clear();
                        response_thinking.clear();
                        calls.clear();
                        streams.clear();
                    }
                    ClientEvent::StateCommitted(state) => {
                        if let CommitCause::ToolRoundStarted(round_id) = &state.cause {
                            active_round = Some(round_id.clone());
                        }
                        let mut tool_round_settled = false;
                        if let CommitCause::ToolResult { call_id } = &state.cause {
                            let completion = completions.remove(call_id).ok_or_else(|| {
                                Error::Protocol(format!(
                                    "core committed a tool result without typed Cursor state: {call_id}"
                                ))
                            })?;
                            let snapshot = self
                                .store
                                .tool_round(active_round.as_ref().ok_or_else(|| {
                                    Error::Protocol("tool commit has no active round".into())
                                })?)
                                .await?
                                .ok_or_else(|| {
                                    Error::Store("active tool round disappeared".into())
                                })?;
                            let call = snapshot
                                .calls
                                .iter()
                                .find(|call| call.call_id == *call_id)
                                .ok_or_else(|| {
                                    Error::Protocol(format!(
                                        "committed call is absent from tool round: {call_id}"
                                    ))
                                })?;
                            self.handle
                                .emit(&interaction::tool_completed(call, &completion))?;
                            presentation.tool_completed(&completion);
                            completed.insert(call_id.clone());
                            tool_round_settled = snapshot.status == ToolRoundStatus::Settled;
                        }
                        let final_turn = state.cause == CommitCause::FinalTurn;
                        if final_turn {
                            if !state.barrier.is_required() {
                                return Err(Error::Protocol(
                                    "final state has no completion barrier".into(),
                                ));
                            }
                            let (sender, receiver) = oneshot::channel();
                            worker
                                .jobs
                                .send(CheckpointJob {
                                    kind: CheckpointKind::Final {
                                        revision_id: state.revision_id,
                                        result: sender,
                                    },
                                    presentation: presentation.take(),
                                    context_tokens,
                                    ready: None,
                                })
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker closed".into()))?;
                            match receiver
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker stopped".into()))?
                            {
                                Ok(checkpoints) => {
                                    final_checkpoint = Some(checkpoints);
                                    state.barrier.complete(Ok(()));
                                }
                                Err(error) => {
                                    state.barrier.complete(Err(error.to_string()));
                                    return Err(error);
                                }
                            }
                        } else if let CommitCause::ToolRoundStarted(round_id) = &state.cause {
                            worker
                                .jobs
                                .send(CheckpointJob {
                                    kind: CheckpointKind::ToolStarted {
                                        round_id: round_id.clone(),
                                        stable_revision_id: state.revision_id,
                                    },
                                    presentation: presentation.take(),
                                    context_tokens,
                                    ready: None,
                                })
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker closed".into()))?;
                        } else if tool_round_settled {
                            if !state.barrier.is_required() {
                                return Err(Error::Protocol(
                                    "settled tool round has no completion barrier".into(),
                                ));
                            }
                            let (ready, published) = oneshot::channel();
                            worker
                                .jobs
                                .send(CheckpointJob {
                                    kind: CheckpointKind::ToolSettled(state.revision_id),
                                    presentation: presentation.take(),
                                    context_tokens,
                                    ready: Some(ready),
                                })
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker closed".into()))?;
                            let result = published
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker stopped".into()))?
                                .map_err(Error::Protocol);
                            match result {
                                Ok(()) => state.barrier.complete(Ok(())),
                                Err(error) => {
                                    state.barrier.complete(Err(error.to_string()));
                                    return Err(error);
                                }
                            }
                            active_round = None;
                            self.tool_runtime.clear_completed().await;
                        } else if !matches!(&state.cause, CommitCause::ToolResult { .. })
                            && active_round.is_some()
                        {
                            let round_id = active_round.clone().ok_or_else(|| {
                                Error::Protocol("active tool round disappeared".into())
                            })?;
                            worker
                                .jobs
                                .send(CheckpointJob {
                                    kind: CheckpointKind::ToolStarted {
                                        round_id,
                                        stable_revision_id: state.revision_id,
                                    },
                                    presentation: presentation.take(),
                                    context_tokens,
                                    ready: None,
                                })
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker closed".into()))?;
                        } else if !matches!(&state.cause, CommitCause::ToolResult { .. }) {
                            let requires_ready = state.barrier.is_required();
                            let (ready, published) = oneshot::channel();
                            worker
                                .jobs
                                .send(CheckpointJob {
                                    kind: CheckpointKind::Settled(state.revision_id),
                                    presentation: presentation.take(),
                                    context_tokens,
                                    ready: requires_ready.then_some(ready),
                                })
                                .await
                                .map_err(|_| Error::Protocol("checkpoint worker closed".into()))?;
                            if requires_ready {
                                let result = published
                                    .await
                                    .map_err(|_| {
                                        Error::Protocol("checkpoint worker stopped".into())
                                    })?
                                    .map_err(Error::Protocol);
                                match result {
                                    Ok(()) => state.barrier.complete(Ok(())),
                                    Err(error) => {
                                        state.barrier.complete(Err(error.to_string()));
                                        return Err(error);
                                    }
                                }
                            }
                        }
                    }
                    ClientEvent::Ended(outcome) => {
                        return match outcome {
                            RunOutcome::Completed => {
                                let checkpoints = final_checkpoint.take().ok_or_else(|| {
                                    Error::Protocol("Completed without final state".into())
                                })?;
                                self.handle.emit(&interaction::turn_ended(turn_usage))?;
                                self.checkpoint
                                    .publish(&self.handle, &checkpoints.staged)
                                    .await?;
                                self.checkpoint
                                    .publish(&self.handle, &checkpoints.settled)
                                    .await?;
                                self.handle.emit(&pb::AgentServerMessage {
                                    ttft_breakdown: None,
                                    message: Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(checkpoints.settled)),
                                })?;
                                crate::cursor::lifecycle::finish_success(&self.handle);
                                Ok(())
                            }
                            RunOutcome::Cancelled => {
                                worker.abort();
                                self.abort_execs().await;
                                crate::cursor::lifecycle::cancel(&self.handle)
                            }
                            RunOutcome::Failed(failure) => {
                                worker.abort();
                                self.abort_execs().await;
                                crate::cursor::lifecycle::fail(&self.handle, &cursor_error(failure))
                            }
                        };
                    }
                },
            }
        }
    }

    async fn abort_execs(&self) {
        for id in self.tool_runtime.drain_running().await {
            let _ = self.handle.emit(&codec::abort(id));
        }
    }

    async fn forward_completion(
        &self,
        completion: ToolCompletion,
        completions: &mut HashMap<String, ToolCompletion>,
    ) -> Result<()> {
        let result = completion.result();
        if result.call_id.is_empty() {
            return Err(Error::Protocol("tool result call_id is empty".into()));
        }
        if completions
            .insert(result.call_id.clone(), completion.clone())
            .is_some()
        {
            return Err(Error::Protocol(format!(
                "duplicate tool result call_id: {}",
                result.call_id
            )));
        }
        self.core
            .commands
            .send(ClientCommand::ToolResult {
                call_id: result.call_id.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
            })
            .await
            .map_err(|_| Error::RunNotFound(self.context.request_id.clone()))
    }

    fn emit_model_event(
        &self,
        event: crate::provider::ModelEvent,
        model_call_id: &str,
    ) -> Result<()> {
        if let Some(message) = interaction::response_event(&event, model_call_id)? {
            self.handle.emit(&message)?;
        }
        Ok(())
    }
}

enum Input {
    Event(Option<ClientEvent>),
    Completion(ToolCompletion),
    CompletionResult(Option<Result<ToolCompletion>>),
    CheckpointFailure(Option<Error>),
}

fn cursor_error(failure: RunFailure) -> Error {
    match failure {
        RunFailure::Protocol(message) => Error::Protocol(message),
        RunFailure::Provider(message) => Error::Provider(message),
        RunFailure::Store(message) => Error::Store(message),
        RunFailure::Client(message) => Error::Protocol(message),
    }
}
