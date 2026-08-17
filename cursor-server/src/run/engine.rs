use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientEvent, ClientPort, CommitBarrier, CommitCause, StateCommitted},
    model::{
        CanonicalMessage, MessageContent, Origin, PreparedRun, Role, RunAction, ToolRoundAssistant,
        ToolRoundId, Usage,
    },
    provider::Provider,
    store::{RunStatus, Store},
};

use super::{consume_model_cycle, ModelCycleFailure, RunFailure, RunOutcome};

pub struct RunEngine {
    store: Store,
    provider: Arc<dyn Provider>,
}

impl RunEngine {
    pub fn new(store: Store, provider: Arc<dyn Provider>) -> Self {
        Self { store, provider }
    }

    #[tracing::instrument(
        skip_all,
        fields(run_id = %prepared.run_id, conversation_id = %prepared.conversation_id)
    )]
    pub async fn run(
        &self,
        prepared: PreparedRun,
        mut client: ClientPort,
        cancellation: CancellationToken,
    ) -> RunOutcome {
        let claimed = match self.store.claim_run(&prepared).await {
            Ok(claimed) => claimed,
            Err(error) => {
                let outcome = RunOutcome::Failed(error.into());
                let _ = client
                    .events
                    .send(ClientEvent::Ended(outcome.clone()))
                    .await;
                tracing::info!(outcome = ?outcome, "Run claim failed");
                return outcome;
            }
        };
        let outcome = self
            .run_claimed(
                &prepared,
                claimed.head_revision_id,
                &mut client,
                &cancellation,
            )
            .await;
        let usage = outcome.1;
        let outcome = outcome.0;
        let (status, failure) = match &outcome {
            RunOutcome::Completed => (RunStatus::Completed, None),
            RunOutcome::Cancelled => (RunStatus::Cancelled, None),
            RunOutcome::Failed(failure) => (
                RunStatus::Failed,
                Some((failure.category(), failure_message(failure))),
            ),
        };
        let failure_ref = failure
            .as_ref()
            .map(|(category, summary)| (*category, summary.as_str()));
        if let Err(error) = self
            .store
            .finish_run(&prepared.run_id, status, usage, failure_ref)
            .await
        {
            tracing::error!(run_id = %prepared.run_id, %error, "failed to persist Run outcome");
        }
        let _ = client
            .events
            .send(ClientEvent::Ended(outcome.clone()))
            .await;
        tracing::info!(outcome = ?outcome, usage = ?usage, "Run ended");
        outcome
    }

    async fn run_claimed(
        &self,
        prepared: &PreparedRun,
        mut revision: crate::model::RevisionId,
        client: &mut ClientPort,
        cancellation: &CancellationToken,
    ) -> (RunOutcome, Option<Usage>) {
        let mut usage = None;
        tracing::info!(
            revision_id = revision.0,
            "Run claimed conversation ownership"
        );
        if !prepared.initial_messages.is_empty() {
            let mut changed = false;
            for message in &prepared.initial_messages {
                match self
                    .store
                    .append_message_once(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        revision,
                        message,
                    )
                    .await
                {
                    Ok((next, inserted)) => {
                        revision = next;
                        changed |= inserted;
                    }
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                }
            }
            if changed {
                let (barrier, ready) = CommitBarrier::before_continue();
                if emit(
                    client,
                    ClientEvent::StateCommitted(StateCommitted {
                        revision_id: revision,
                        tool_round_version: 0,
                        cause: CommitCause::InitialMessages,
                        barrier,
                    }),
                )
                .await
                .is_err()
                {
                    return (client_failure(), usage);
                }
                if let Err(outcome) = wait_for_state_ready(ready, cancellation).await {
                    return (outcome, usage);
                }
            }
        }

        if let RunAction::Resume {
            pending_tool_round: Some(round),
        } = &prepared.action
        {
            revision = match super::tool_round::execute(
                &self.store,
                prepared,
                client,
                cancellation,
                revision,
                super::tool_round::ToolRound {
                    id: ToolRoundId::new(format!("{}:round:resume", prepared.run_id)),
                    assistant: round.assistant.clone(),
                    calls: round.calls.clone(),
                    recovered_started_at_ms: Some(round.started_at_ms),
                },
            )
            .await
            {
                Ok(revision) => revision,
                Err(outcome) => return (outcome, usage),
            };
        }

        loop {
            if cancellation.is_cancelled() {
                return (RunOutcome::Cancelled, usage);
            }
            let messages = match self.store.load_revision_messages(revision).await {
                Ok(messages) => messages,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            let provider_call_index = match self.store.begin_provider_call(&prepared.run_id).await {
                Ok(index) => index,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            tracing::debug!(
                provider_call_index,
                revision_id = revision.0,
                "starting model call"
            );
            let history = match crate::model::project_messages(&messages) {
                Ok(history) => history,
                Err(error) => return (RunOutcome::Failed(error.into()), usage),
            };
            let request = crate::model::ModelRequest {
                prompt: prepared.prompt.clone(),
                model: prepared.model.clone(),
                history,
            };
            let invocation = crate::model::ModelInvocation {
                call_id: format!("{}:{provider_call_index}", prepared.run_id),
                run_id: prepared.run_id.to_string(),
                conversation_id: prepared.conversation_id.to_string(),
                provider_call_index,
                request,
            };
            let cycle = consume_model_cycle(
                self.provider.stream(invocation, cancellation.clone()),
                &client.events,
                cancellation,
            )
            .await;
            let cycle = match cycle {
                Ok(cycle) => cycle,
                Err(ModelCycleFailure {
                    failure,
                    usage: cycle_usage,
                    ..
                }) => {
                    if let Some(cycle_usage) = cycle_usage {
                        accumulate_usage(&mut usage, cycle_usage);
                    }
                    if cancellation.is_cancelled() {
                        return (RunOutcome::Cancelled, usage);
                    }
                    return (RunOutcome::Failed(failure), usage);
                }
            };
            if let Some(cycle_usage) = cycle.usage {
                accumulate_usage(&mut usage, cycle_usage);
            }

            if cycle.calls.is_empty() {
                let assistant = CanonicalMessage {
                    message_id: format!("{}:assistant:{provider_call_index}", prepared.run_id),
                    role: Role::Assistant,
                    origin: Origin::Assistant,
                    content: MessageContent::Assistant {
                        text: cycle.text,
                        thinking: cycle.reasoning,
                        tool_round_id: None,
                        replay_state: cycle.replay_state,
                        tool_calls: Vec::new(),
                    },
                    runtime_event_id: None,
                };
                revision = match self
                    .store
                    .append_revision(
                        &prepared.conversation_id,
                        &prepared.run_id,
                        revision,
                        &[assistant],
                    )
                    .await
                {
                    Ok(revision) => revision,
                    Err(error) => return (RunOutcome::Failed(error.into()), usage),
                };
                let (barrier, ready) = CommitBarrier::before_continue();
                if emit(
                    client,
                    ClientEvent::StateCommitted(StateCommitted {
                        revision_id: revision,
                        tool_round_version: 0,
                        cause: CommitCause::FinalTurn,
                        barrier,
                    }),
                )
                .await
                .is_err()
                {
                    return (client_failure(), usage);
                }
                if let Err(outcome) = wait_for_state_ready(ready, cancellation).await {
                    return (outcome, usage);
                }
                return (RunOutcome::Completed, usage);
            }

            let round_id =
                ToolRoundId::new(format!("{}:round:{provider_call_index}", prepared.run_id));
            revision = match super::tool_round::execute(
                &self.store,
                prepared,
                client,
                cancellation,
                revision,
                super::tool_round::ToolRound {
                    id: round_id,
                    assistant: ToolRoundAssistant {
                        text: cycle.text,
                        thinking: cycle.reasoning,
                        model_call_id: cycle.model_call_id,
                        replay_state: cycle.replay_state,
                    },
                    calls: cycle.calls,
                    recovered_started_at_ms: None,
                },
            )
            .await
            {
                Ok(revision) => revision,
                Err(outcome) => return (outcome, usage),
            };
        }
    }
}

fn accumulate_usage(total: &mut Option<Usage>, usage: Usage) {
    match total {
        Some(total) => *total += usage,
        None => *total = Some(usage),
    }
}

pub(super) async fn wait_for_state_ready(
    ready: tokio::sync::oneshot::Receiver<std::result::Result<(), String>>,
    cancellation: &CancellationToken,
) -> std::result::Result<(), RunOutcome> {
    let result = tokio::select! {
        biased;
        result = ready => result,
        _ = cancellation.cancelled() => return Err(RunOutcome::Cancelled),
    };
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(RunOutcome::Failed(RunFailure::Client(error))),
        Err(_) => Err(client_failure()),
    }
}

async fn emit(client: &ClientPort, event: ClientEvent) -> Result<(), ()> {
    client.events.send(event).await.map_err(|_| ())
}

fn client_failure() -> RunOutcome {
    RunOutcome::Failed(RunFailure::Client("client event channel closed".into()))
}

fn failure_message(failure: &RunFailure) -> String {
    match failure {
        RunFailure::Protocol(message)
        | RunFailure::Provider(message)
        | RunFailure::Store(message)
        | RunFailure::Client(message) => message.clone(),
    }
}
