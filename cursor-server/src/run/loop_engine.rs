use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};

use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    cursor::{
        blob_sync::BlobSynchronizer,
        checkpoint::CheckpointBuilder,
        exec, interaction,
        pending::{ExecContext, PendingExecRegistry},
        proto::agent::v1 as pb,
        tool_result::ToolResultReceiver,
        tools::ToolDispatcher,
    },
    model::{
        CanonicalMessage, MessageContent, Origin, Role, RuntimeEvent, ToolCall, ToolCallContent,
        ToolResultContent, Usage,
    },
    prompting::{Mode, PromptCompiler, ToolDefinition},
    provider::{FinishReason, Provider, ResponseEvent},
    store::{RunStatus, Store},
    Error, Result,
};

use super::{actor::RunDependencies, lifecycle, RunCommand, RunHandle};

pub struct LoopEngine {
    handle: RunHandle,
    store: Store,
    provider: Arc<dyn Provider>,
    compiler: PromptCompiler,
    default_model: String,
    blob_sync: BlobSynchronizer,
    results: ToolResultReceiver,
    tools: ToolDispatcher,
    pending_execs: PendingExecRegistry,
    turn_usage: Usage,
}

impl LoopEngine {
    pub(crate) fn new(
        handle: RunHandle,
        dependencies: RunDependencies,
        blob_sync: BlobSynchronizer,
        results: ToolResultReceiver,
        tools: ToolDispatcher,
        pending_execs: PendingExecRegistry,
    ) -> Self {
        Self {
            handle,
            store: dependencies.store,
            provider: dependencies.provider,
            compiler: dependencies.compiler,
            default_model: dependencies.model,
            blob_sync,
            results,
            tools,
            pending_execs,
            turn_usage: Usage::default(),
        }
    }

    pub async fn run(mut self, request: pb::AgentRunRequest) {
        if let Err(error) = self.run_inner(request).await {
            let status = if matches!(error, Error::Cancelled) {
                RunStatus::Interrupted
            } else {
                RunStatus::Failed
            };
            if status == RunStatus::Failed {
                tracing::error!(
                    request_id = self.handle.request_id(),
                    error = %error,
                    "run failed"
                );
            } else {
                tracing::info!(
                    request_id = self.handle.request_id(),
                    error = %error,
                    "run interrupted"
                );
            }
            let _ = self
                .store
                .update_run_status(self.handle.request_id(), status, self.turn_usage)
                .await;
            if matches!(error, Error::Cancelled) {
                self.handle.cancel();
            } else {
                for id in self.pending_execs.drain_running().await {
                    let _ = self.handle.emit(&exec::abort(id));
                }
                if let Err(terminal_error) = lifecycle::fail(&self.handle, &error) {
                    tracing::error!(%terminal_error, "failed to encode RunSSE terminal error");
                    self.handle.close_output();
                }
                let _ = self.handle.command(RunCommand::Finished).await;
            }
        } else {
            let _ = self.handle.command(RunCommand::Finished).await;
        }
    }

    async fn run_inner(&mut self, request: pb::AgentRunRequest) -> Result<()> {
        let conversation_id = request
            .conversation_id
            .clone()
            .unwrap_or_else(|| self.handle.request_id().into());
        let run_id = request
            .run_id
            .clone()
            .unwrap_or_else(|| self.handle.request_id().into());
        let exec_context = exec_context(&request, &conversation_id);
        let revision = self.store.begin_revision(&conversation_id).await?;
        self.store
            .bind_run(
                self.handle.request_id(),
                &run_id,
                &conversation_id,
                revision,
            )
            .await?;
        let checkpoint = CheckpointBuilder::new(self.store.clone(), self.blob_sync.clone());
        checkpoint
            .import_prefetched(&request.pre_fetched_blobs)
            .await?;
        let local = self.store.load_messages(&conversation_id).await?;
        let mut messages = if local.is_empty() {
            checkpoint
                .hydrate_messages(request.conversation_state.as_ref())
                .await?
        } else {
            local
        };

        let (mode_number, user_messages, runtime_tag) = extract_action(&request)?;
        if !messages.is_empty() && self.store.load_messages(&conversation_id).await?.is_empty() {
            self.store
                .append_messages(&conversation_id, &messages)
                .await?;
        }
        let static_context = static_context_text(&request);
        let context_hash = content_hash(&static_context);
        let latest_context_hash = messages.iter().rev().find_map(message_context_hash);
        if latest_context_hash.is_none() && !static_context.is_empty() {
            let id = format!("request-context:{context_hash}");
            let message =
                CanonicalMessage::text(id, Role::System, Origin::Prompt, static_context.clone());
            self.store
                .append_messages(&conversation_id, std::slice::from_ref(&message))
                .await?;
            messages.push(message);
        }
        for message in user_messages {
            self.store
                .append_messages(&conversation_id, std::slice::from_ref(&message))
                .await?;
            messages.push(message);
        }
        if latest_context_hash
            .as_deref()
            .is_some_and(|latest| latest != context_hash)
        {
            let event_id = format!(
                "request-context-event:{}:{context_hash}",
                request
                    .run_id
                    .as_deref()
                    .unwrap_or(self.handle.request_id())
            );
            let event = RuntimeEvent {
                event_id,
                text: format!("<runtime_context>\n{static_context}\n</runtime_context>"),
            };
            let message = event.clone().into_message();
            if self
                .store
                .append_runtime_event_once(&conversation_id, event)
                .await?
            {
                messages.push(message);
            }
        }
        if let Some(selected) = selected_context_text(&request).filter(|text| !text.is_empty()) {
            let user_id = request
                .action
                .as_ref()
                .and_then(|action| action.action.as_ref())
                .and_then(|action| match action {
                    pb::conversation_action::Action::UserMessageAction(action) => action
                        .user_message
                        .as_ref()
                        .map(|user| user.message_id.as_str()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    request
                        .run_id
                        .as_deref()
                        .unwrap_or(self.handle.request_id())
                });
            let event_id = format!("selected-context:{user_id}:{}", content_hash(&selected));
            let event = RuntimeEvent {
                event_id,
                text: format!("<selected_context>\n{selected}\n</selected_context>"),
            };
            let message = event.clone().into_message();
            if self
                .store
                .append_runtime_event_once(&conversation_id, event)
                .await?
            {
                messages.push(message);
            }
        }
        if let Some(event) = runtime_tag {
            let event_message = event.clone().into_message();
            if self
                .store
                .append_runtime_event_once(&conversation_id, event)
                .await?
            {
                messages.push(event_message);
            }
        }

        let mode = if request.subagent_type_name.is_some() {
            Mode::Subagent
        } else {
            mode_from_proto(mode_number)
        };
        let model = request
            .requested_model
            .as_ref()
            .map(|model| model.model_id.clone())
            .filter(|model| !model.is_empty())
            .or_else(|| {
                request
                    .model_details
                    .as_ref()
                    .map(|model| model.model_id.clone())
                    .filter(|model| !model.is_empty())
            })
            .unwrap_or_else(|| self.default_model.clone());
        let dynamic_mcp = dynamic_mcp_tools(&request)?;
        let dynamic_tool_definitions = dynamic_mcp
            .values()
            .map(|(_, definition)| definition.clone())
            .collect::<Vec<_>>();
        let dynamic_cursor_tools = dynamic_mcp
            .iter()
            .map(|(name, (wire, _))| (name.clone(), wire.clone()))
            .collect();
        let mut call_index = 0usize;
        loop {
            if self.handle.cancellation().is_cancelled() {
                return Err(Error::Cancelled);
            }
            let model_call_id = format!("{}:{call_index}", self.handle.request_id());
            let model_request = self.compiler.compile_with_dynamic_tools(
                mode,
                model.clone(),
                model_call_id.clone(),
                &messages,
                &dynamic_tool_definitions,
            )?;
            self.store
                .begin_provider_call(self.handle.request_id(), call_index)
                .await?;
            tracing::info!(
                request_id = self.handle.request_id(),
                conversation_id,
                call_index,
                model,
                message_count = model_request.messages.len(),
                "provider call started"
            );
            let mut stream = self
                .provider
                .stream(model_request, self.handle.cancellation());
            let mut text = String::new();
            let mut thinking = String::new();
            let mut calls = BTreeMap::<usize, ToolCall>::new();
            let mut call_usage = Usage::default();
            let mut finish = FinishReason::Error;
            let mut thinking_started_at: Option<Instant> = None;
            while let Some(event) = stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        if let Some(started_at) = thinking_started_at.take() {
                            self.handle
                                .emit(&interaction::thinking_completed(started_at.elapsed()))?;
                        }
                        self.turn_usage += call_usage;
                        if let Err(checkpoint_error) = self
                            .checkpoint_failed_model_call(
                                &conversation_id,
                                revision,
                                &mut messages,
                                mode_number,
                                call_index,
                                text,
                                thinking,
                                &checkpoint,
                            )
                            .await
                        {
                            if matches!(checkpoint_error, Error::Cancelled) {
                                return Err(checkpoint_error);
                            }
                            tracing::warn!(
                                %checkpoint_error,
                                "failed to publish checkpoint before RunSSE error"
                            );
                        }
                        return Err(error);
                    }
                };
                match &event {
                    ResponseEvent::TextDelta(delta) => {
                        text.push_str(delta);
                        self.handle.emit(
                            &interaction::response_event(&event, &model_call_id)?
                                .expect("text delta is visible"),
                        )?;
                    }
                    ResponseEvent::ThinkingStart => {
                        if thinking_started_at.replace(Instant::now()).is_some() {
                            return Err(Error::Protocol(
                                "provider started thinking while thinking was active".into(),
                            ));
                        }
                    }
                    ResponseEvent::ThinkingDelta(delta) => {
                        if thinking_started_at.is_none() {
                            return Err(Error::Protocol(
                                "provider emitted thinking delta before thinking start".into(),
                            ));
                        }
                        thinking.push_str(delta);
                        self.handle.emit(
                            &interaction::response_event(&event, &model_call_id)?
                                .expect("thinking delta is visible"),
                        )?;
                    }
                    ResponseEvent::ThinkingEnd => {
                        let started_at = thinking_started_at.take().ok_or_else(|| {
                            Error::Protocol("provider ended thinking before thinking start".into())
                        })?;
                        self.handle
                            .emit(&interaction::thinking_completed(started_at.elapsed()))?;
                    }
                    ResponseEvent::ToolCallStart {
                        index,
                        call_id,
                        name,
                    } => {
                        let call = ToolCall {
                            index: *index,
                            call_id: call_id.clone(),
                            model_call_id: model_call_id.clone(),
                            name: name.clone(),
                            arguments_text: String::new(),
                            arguments: Value::Null,
                        };
                        self.handle.emit(
                            &interaction::response_event(&event, &model_call_id)?
                                .expect("tool start is visible"),
                        )?;
                        calls.insert(*index, call);
                    }
                    ResponseEvent::ToolCallArgumentsDelta { index, delta } => {
                        if let Some(call) = calls.get_mut(index) {
                            call.arguments_text.push_str(delta);
                            self.handle
                                .emit(&interaction::arguments_delta(call, delta)?)?;
                        }
                    }
                    ResponseEvent::Usage(usage) => merge_usage(&mut call_usage, *usage),
                    ResponseEvent::Done(reason) => finish = *reason,
                    _ => {
                        if let Some(message) = interaction::response_event(&event, &model_call_id)?
                        {
                            self.handle.emit(&message)?;
                        }
                    }
                }
            }
            self.turn_usage += call_usage;
            tracing::info!(
                request_id = self.handle.request_id(),
                conversation_id,
                call_index,
                ?finish,
                tool_count = calls.len(),
                input_tokens = call_usage.input_tokens,
                output_tokens = call_usage.output_tokens,
                "provider call completed"
            );
            for call in calls.values_mut() {
                call.arguments = serde_json::from_str(&call.arguments_text).map_err(|error| {
                    Error::Protocol(format!("invalid arguments for tool {}: {error}", call.name))
                })?;
            }
            if finish == FinishReason::Aborted {
                return Err(Error::Cancelled);
            }
            if calls.is_empty() {
                let assistant = assistant_message(
                    format!("{}:assistant:{call_index}", self.handle.request_id()),
                    model_call_id.clone(),
                    text,
                    thinking,
                    &[],
                );
                self.store
                    .append_messages(&conversation_id, std::slice::from_ref(&assistant))
                    .await?;
                messages.push(assistant);
                let state = checkpoint
                    .build(&conversation_id, revision, &messages, mode_number)
                    .await?;
                lifecycle::publish_success(
                    &self.handle,
                    self.turn_usage,
                    &checkpoint,
                    Some(&state),
                )
                .await?;
                self.store
                    .update_run_status(
                        self.handle.request_id(),
                        RunStatus::Completed,
                        self.turn_usage,
                    )
                    .await?;
                lifecycle::finish_success(&self.handle);
                return Ok(());
            }

            let ordered_calls = calls.into_values().collect::<Vec<_>>();
            let intent_state = checkpoint
                .build_with_tool_progress(
                    &conversation_id,
                    revision,
                    &messages,
                    mode_number,
                    &ordered_calls,
                    &std::collections::HashSet::new(),
                )
                .await?;
            checkpoint.publish(&self.handle, &intent_state).await?;

            let mut completed = HashSet::new();
            let mut response_text = text;
            let mut response_thinking = thinking;
            for recovered in self
                .store
                .load_tool_results(self.handle.request_id(), call_index)
                .await?
            {
                let call_position = ordered_calls
                    .iter()
                    .position(|call| call.call_id == recovered.call_id)
                    .ok_or_else(|| {
                        Error::Protocol(format!(
                            "recovered unknown tool result call_id: {}",
                            recovered.call_id
                        ))
                    })?;
                if completed.insert(recovered.call_id.clone()) {
                    self.append_tool_pair(
                        &conversation_id,
                        &mut messages,
                        call_index,
                        call_position,
                        &ordered_calls[call_position],
                        &recovered,
                        std::mem::take(&mut response_text),
                        std::mem::take(&mut response_thinking),
                    )
                    .await?;
                    let state = checkpoint
                        .build_with_tool_progress(
                            &conversation_id,
                            revision,
                            &messages,
                            mode_number,
                            &ordered_calls,
                            &completed,
                        )
                        .await?;
                    checkpoint.publish(&self.handle, &state).await?;
                }
            }
            let mut ready = VecDeque::new();
            for dispatched in self
                .tools
                .start_batch(
                    &ordered_calls,
                    &completed,
                    &messages,
                    &response_text,
                    &response_thinking,
                    &dynamic_cursor_tools,
                    &exec_context,
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
            while completed.len() < ordered_calls.len() {
                let completion = if let Some(completion) = ready.pop_front() {
                    completion
                } else {
                    let cancellation = self.handle.cancellation();
                    tokio::select! {
                        result = self.results.recv() => {
                            result.ok_or_else(|| Error::Protocol("tool result channel closed".into()))??
                        },
                        _ = cancellation.cancelled() => return Err(Error::Cancelled),
                    }
                };
                let result = completion.result();
                let call_position = ordered_calls
                    .iter()
                    .position(|call| call.call_id == result.call_id)
                    .ok_or_else(|| {
                        Error::Protocol(format!("unknown tool result call_id: {}", result.call_id))
                    })?;
                if completed.insert(result.call_id.clone()) {
                    self.store
                        .save_tool_result(
                            self.handle.request_id(),
                            call_index,
                            call_position,
                            result,
                        )
                        .await?;
                    self.append_tool_pair(
                        &conversation_id,
                        &mut messages,
                        call_index,
                        call_position,
                        &ordered_calls[call_position],
                        result,
                        std::mem::take(&mut response_text),
                        std::mem::take(&mut response_thinking),
                    )
                    .await?;
                    let state = checkpoint
                        .build_with_tool_progress(
                            &conversation_id,
                            revision,
                            &messages,
                            mode_number,
                            &ordered_calls,
                            &completed,
                        )
                        .await?;
                    checkpoint.publish(&self.handle, &state).await?;
                    self.handle.emit(&interaction::tool_completed(
                        &ordered_calls[call_position],
                        &completion,
                    ))?;
                }
            }
            self.store
                .clear_tool_results(self.handle.request_id(), call_index)
                .await?;
            call_index += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn checkpoint_failed_model_call(
        &self,
        conversation_id: &str,
        revision: i64,
        messages: &mut Vec<CanonicalMessage>,
        mode: i32,
        call_index: usize,
        text: String,
        thinking: String,
        checkpoint: &CheckpointBuilder,
    ) -> Result<()> {
        if !text.is_empty() || !thinking.is_empty() {
            let partial = assistant_message(
                format!(
                    "{}:assistant:{call_index}:partial",
                    self.handle.request_id()
                ),
                format!("{}:{call_index}", self.handle.request_id()),
                text,
                thinking,
                &[],
            );
            self.store
                .append_messages(conversation_id, std::slice::from_ref(&partial))
                .await?;
            messages.push(partial);
        }
        let state = checkpoint
            .build(conversation_id, revision, messages, mode)
            .await?;
        checkpoint.publish(&self.handle, &state).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn append_tool_pair(
        &self,
        conversation_id: &str,
        messages: &mut Vec<CanonicalMessage>,
        batch_index: usize,
        call_position: usize,
        call: &ToolCall,
        result: &crate::model::ToolResult,
        text: String,
        thinking: String,
    ) -> Result<()> {
        let assistant = assistant_message(
            format!(
                "{}:assistant:{batch_index}:{}",
                self.handle.request_id(),
                call.call_id
            ),
            call.model_call_id.clone(),
            text,
            thinking,
            std::slice::from_ref(call),
        );
        let tool_result = CanonicalMessage {
            message_id: format!(
                "{}:tool:{batch_index}:{call_position}:{}",
                self.handle.request_id(),
                call.call_id
            ),
            role: Role::Tool,
            origin: Origin::Tool,
            content: MessageContent::ToolResult(ToolResultContent {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                output: result.output.clone(),
                is_error: result.is_error,
            }),
            runtime_event_id: None,
        };
        let pair = [assistant, tool_result];
        self.store.append_messages(conversation_id, &pair).await?;
        messages.extend(pair);
        Ok(())
    }
}

fn assistant_message(
    id: String,
    model_call_id: String,
    text: String,
    thinking: String,
    calls: &[ToolCall],
) -> CanonicalMessage {
    CanonicalMessage {
        message_id: id,
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text,
            thinking,
            model_call_id: Some(model_call_id),
            tool_calls: calls
                .iter()
                .map(|call| ToolCallContent {
                    index: call.index,
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
        },
        runtime_event_id: None,
    }
}

fn extract_action(
    request: &pb::AgentRunRequest,
) -> Result<(i32, Vec<CanonicalMessage>, Option<RuntimeEvent>)> {
    let action = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref());
    let Some(pb::conversation_action::Action::UserMessageAction(action)) = action else {
        return Ok((
            request
                .conversation_state
                .as_ref()
                .and_then(|state| state.mode)
                .unwrap_or(pb::AgentMode::Agent as i32),
            Vec::new(),
            None,
        ));
    };
    let mut output = Vec::new();
    for user in action
        .prepend_user_messages
        .iter()
        .chain(action.user_message.iter())
    {
        output.push(CanonicalMessage::text(
            user.message_id.clone(),
            Role::User,
            Origin::User,
            user.text.clone(),
        ));
    }
    let user = action.user_message.as_ref();
    let mode = user
        .map(|user| user.mode)
        .unwrap_or(pb::AgentMode::Agent as i32);
    let runtime = user
        .and_then(|user| user.subagent_system_reminder.clone())
        .map(|text| RuntimeEvent {
            event_id: format!(
                "{}:subagent-system-reminder",
                request.run_id.as_deref().unwrap_or("run")
            ),
            text,
        });
    Ok((mode, output, runtime))
}

fn mode_from_proto(mode: i32) -> Mode {
    match pb::AgentMode::try_from(mode).unwrap_or(pb::AgentMode::Agent) {
        pb::AgentMode::Ask => Mode::Ask,
        pb::AgentMode::Plan => Mode::Plan,
        pb::AgentMode::Debug => Mode::Debug,
        pb::AgentMode::Multitask => Mode::Multitask,
        _ => Mode::Agent,
    }
}

fn merge_usage(current: &mut Usage, value: Usage) {
    current.input_tokens = current.input_tokens.max(value.input_tokens);
    current.output_tokens = current.output_tokens.max(value.output_tokens);
    current.cache_read_tokens = current.cache_read_tokens.max(value.cache_read_tokens);
    current.cache_write_tokens = current.cache_write_tokens.max(value.cache_write_tokens);
    current.reasoning_tokens = current.reasoning_tokens.max(value.reasoning_tokens);
}

fn static_context_text(request: &pb::AgentRunRequest) -> String {
    let mut sections = Vec::new();
    if let Some(custom) = request
        .custom_system_prompt
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        sections.push(format!("## Custom system instructions\n{custom}"));
    }
    if let Some(kind) = request.subagent_type_name.as_deref() {
        sections.push(format!("## Subagent type\n{kind}"));
    }
    if let Some(options) = &request.skill_options {
        let body = options
            .skill_descriptors
            .iter()
            .filter(|skill| skill.enabled)
            .map(|skill| {
                format!(
                    "- {} ({})\n  {}",
                    skill.name, skill.folder_path, skill.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !body.is_empty() {
            sections.push(format!("## Available skills\n{body}"));
        }
    }
    let context = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
        .and_then(|action| match action {
            pb::conversation_action::Action::UserMessageAction(action) => {
                action.request_context.as_ref()
            }
            _ => None,
        });
    if let Some(context) = context {
        let rules = context
            .rules
            .iter()
            .chain(context.non_file_rules.iter())
            .map(|rule| format!("### {}\n{}", rule.full_path, rule.content))
            .collect::<Vec<_>>()
            .join("\n");
        if !rules.is_empty() {
            sections.push(format!("## Rules\n{rules}"));
        }
        let skills = context
            .agent_skills
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .map(|skill| {
                format!(
                    "### {}\n{}\n{}",
                    skill.full_path, skill.description, skill.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !skills.is_empty() {
            sections.push(format!("## Skills\n{skills}"));
        }
        let subagents = context
            .custom_subagents
            .iter()
            .map(|agent| {
                format!(
                    "- {}: {} (model={}, tools={})",
                    agent.name,
                    agent.description,
                    agent.model,
                    agent.tools.join(",")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !subagents.is_empty() {
            sections.push(format!("## Subagents\n{subagents}"));
        }
        let mcp = context
            .mcp_instructions
            .iter()
            .map(|item| {
                format!(
                    "### {} ({})\n{}",
                    item.server_name, item.server_identifier, item.instructions
                )
            })
            .chain(context.tools.iter().map(|tool| {
                format!(
                    "- MCP tool {} / {}: {}",
                    tool.provider_identifier, tool.name, tool.description
                )
            }))
            .collect::<Vec<_>>()
            .join("\n");
        if !mcp.is_empty() {
            sections.push(format!("## MCP\n{mcp}"));
        }
        if let Some(env) = &context.env {
            sections.push(format!("## Runtime environment\nOS: {}\nShell: {}\nTimezone: {}\nProject: {}\nTerminals: {}\nWorkspaces:\n{}",
                env.os_version, env.shell, env.time_zone, env.project_folder, env.terminals_folder, env.workspace_paths.join("\n")));
        }
        if !context.admin_command_denylist.is_empty() {
            sections.push(format!(
                "## Forbidden commands\n{}",
                context.admin_command_denylist.join("\n")
            ));
        }
    }
    if let Some(mcp) = &request.mcp_tools {
        let body = mcp
            .mcp_tools
            .iter()
            .map(|tool| {
                format!(
                    "- {} / {}: {}",
                    tool.provider_identifier, tool.name, tool.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !body.is_empty() {
            sections.push(format!("## Run MCP tools\n{body}"));
        }
    }
    sections.join("\n\n")
}

fn exec_context(request: &pb::AgentRunRequest, conversation_id: &str) -> ExecContext {
    let context = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
        .and_then(|action| match action {
            pb::conversation_action::Action::UserMessageAction(action) => {
                action.request_context.as_ref()
            }
            _ => None,
        });
    ExecContext {
        conversation_id: conversation_id.into(),
        terminals_folder: context
            .and_then(|context| context.env.as_ref())
            .map(|env| env.terminals_folder.clone())
            .unwrap_or_default(),
        admin_command_denylist: context
            .map(|context| context.admin_command_denylist.clone())
            .unwrap_or_default(),
    }
}

fn selected_context_text(request: &pb::AgentRunRequest) -> Option<String> {
    let action = request.action.as_ref()?.action.as_ref()?;
    let pb::conversation_action::Action::UserMessageAction(action) = action else {
        return None;
    };
    let selected = action.user_message.as_ref()?.selected_context.as_ref()?;
    let mut sections = selected.extra_context.clone();
    sections.extend(
        selected
            .files
            .iter()
            .map(|file| format!("<file path=\"{}\">\n{}\n</file>", file.path, file.content)),
    );
    sections.extend(selected.code_selections.iter().map(|selection| {
        format!(
            "<code path=\"{}\">\n{}\n</code>",
            selection.path, selection.content
        )
    }));
    sections.extend(selected.terminals.iter().map(|terminal| {
        format!(
            "<terminal title=\"{}\">\n{}\n</terminal>",
            terminal.title.as_deref().unwrap_or_default(),
            terminal.content
        )
    }));
    sections.extend(selected.terminal_selections.iter().map(|terminal| {
        format!(
            "<terminal_selection title=\"{}\">\n{}\n</terminal_selection>",
            terminal.title.as_deref().unwrap_or_default(),
            terminal.content
        )
    }));
    sections.extend(
        selected
            .cursor_rules
            .iter()
            .filter_map(|selected| selected.rule.as_ref())
            .map(|rule| {
                format!(
                    "<rule path=\"{}\">\n{}\n</rule>",
                    rule.full_path, rule.content
                )
            }),
    );
    sections.extend(selected.cursor_commands.iter().map(|command| {
        format!(
            "<command name=\"{}\">\n{}\n</command>",
            command.name, command.content
        )
    }));
    sections.extend(selected.selected_skills.iter().map(|skill| {
        format!(
            "<skill path=\"{}\">\n{}\n{}\n</skill>",
            skill.full_path, skill.description, skill.content
        )
    }));
    sections.extend(selected.external_links.iter().map(|link| {
        format!(
            "External link: {}{}",
            link.url,
            link.pdf_content
                .as_deref()
                .map(|content| format!("\n{content}"))
                .unwrap_or_default()
        )
    }));
    Some(sections.join("\n\n"))
}

fn dynamic_mcp_tools(
    request: &pb::AgentRunRequest,
) -> Result<BTreeMap<String, (pb::McpToolDefinition, ToolDefinition)>> {
    let request_tools = request
        .mcp_tools
        .iter()
        .flat_map(|tools| tools.mcp_tools.iter());
    let context_tools = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
        .and_then(|action| match action {
            pb::conversation_action::Action::UserMessageAction(action) => {
                action.request_context.as_ref()
            }
            _ => None,
        })
        .into_iter()
        .flat_map(|context| context.tools.iter());
    let mut output = BTreeMap::new();
    for wire in request_tools.chain(context_tools) {
        if wire.name.is_empty() {
            return Err(Error::Protocol(
                "MCP tool definition is missing name".into(),
            ));
        }
        let name = wire.name.clone();
        let input_schema = match wire.input_schema_json.as_deref() {
            Some(json) if !json.trim().is_empty() => serde_json::from_str(json)?,
            _ => prost_value_to_json(wire.input_schema.as_ref().ok_or_else(|| {
                Error::Protocol(format!("MCP tool {name} is missing input schema"))
            })?),
        };
        let definition = ToolDefinition {
            name: name.clone(),
            description: wire.description.clone(),
            input_schema,
        };
        if output
            .insert(name.clone(), (wire.clone(), definition))
            .is_some()
        {
            return Err(Error::Protocol(format!(
                "duplicate MCP tool definition: {name}"
            )));
        }
    }
    Ok(output)
}

fn prost_value_to_json(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match value.kind.as_ref() {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(value)) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(Kind::StringValue(value)) => Value::String(value.clone()),
        Some(Kind::BoolValue(value)) => Value::Bool(*value),
        Some(Kind::StructValue(value)) => Value::Object(
            value
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), prost_value_to_json(value)))
                .collect(),
        ),
        Some(Kind::ListValue(value)) => {
            Value::Array(value.values.iter().map(prost_value_to_json).collect())
        }
    }
}

fn content_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn message_context_hash(message: &CanonicalMessage) -> Option<String> {
    message
        .message_id
        .strip_prefix("request-context:")
        .map(str::to_string)
        .or_else(|| {
            message
                .runtime_event_id
                .as_deref()?
                .strip_prefix("request-context-event:")?
                .rsplit(':')
                .next()
                .map(str::to_string)
        })
}
