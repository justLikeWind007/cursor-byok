use std::collections::BTreeMap;

use crate::{
    cursor::prompting::{Mode, PromptCompiler},
    cursor::{
        blob_sync::BlobSynchronizer,
        checkpoint::CheckpointBuilder,
        projection,
        proto::agent::v1 as pb,
        tools::runtime::{ExecContext, SubagentModel},
    },
    model::{
        CanonicalMessage, ContentPart, ConversationId, MessageContent, Origin, PreparedRun, Role,
        RunAction, RunId, RunKind,
    },
    store::Store,
    Error, Result,
};

use super::{background, context, model, runtime};

struct ActionProjection {
    mode: i32,
    turn_user: Option<pb::UserMessage>,
    action_context: String,
    event_id: Option<String>,
    input_id: Option<String>,
    starts_turn: bool,
}

pub struct CursorRunContext {
    pub request_id: String,
    pub mode: i32,
    pub turn_user: Option<pb::UserMessage>,
    pub exec: ExecContext,
    pub dynamic_tools: BTreeMap<String, pb::McpToolDefinition>,
}

pub(crate) struct PrepareDependencies<'a> {
    pub compiler: &'a PromptCompiler,
    pub store: &'a Store,
    pub checkpoint: &'a CheckpointBuilder,
    pub blob_sync: &'a BlobSynchronizer,
}

pub(crate) async fn prepare(
    request_id: &str,
    request: &pb::AgentRunRequest,
    parent: Option<(RunId, String)>,
    dependencies: PrepareDependencies<'_>,
) -> Result<(PreparedRun, CursorRunContext)> {
    let PrepareDependencies {
        compiler,
        store,
        checkpoint,
        blob_sync,
    } = dependencies;
    checkpoint
        .import_prefetched(&request.pre_fetched_blobs)
        .await?;
    let conversation_id = ConversationId::new(
        request
            .conversation_id
            .clone()
            .unwrap_or_else(|| request_id.into()),
    );
    // RunSSE/Bidi request_id identifies this concrete execution attempt. Cursor may
    // reuse AgentRunRequest.run_id when a queued or subagent-driven attempt resumes.
    let run_id = RunId::new(request_id);
    let mut base_messages = if request.conversation_state.is_some() {
        Some(
            checkpoint
                .hydrate_messages(request.conversation_state.as_ref())
                .await?,
        )
    } else {
        None
    };
    let request_context = context::hydrate(request, blob_sync).await?;
    let ActionProjection {
        mode: mode_number,
        turn_user,
        action_context,
        event_id,
        input_id,
        starts_turn,
    } = action(request_id, request)?;
    let mode = if request.subagent_type_name.is_some() {
        Mode::Subagent
    } else {
        mode_from_proto(mode_number)?
    };
    let model = model::requested_model(request)?;
    let dynamic = context::dynamic_mcp(request, &request_context)?;
    let prompt = compiler.prompt_spec(
        mode,
        &model.model_id,
        &dynamic
            .values()
            .map(|(_, definition)| definition.clone())
            .collect::<Vec<_>>(),
        request.suppress_subagent_progress_update_tool == Some(true),
    )?;
    let proposed_base_revision_id = match base_messages.as_mut() {
        Some(messages) if !messages.is_empty() => {
            validate_prompt_root(messages)?;
            messages.retain(|message| {
                !(message.role == Role::System && message.origin == Origin::Prompt)
            });
            store.import_revision(&conversation_id, messages).await?
        }
        Some(_) | None => store.ensure_conversation(&conversation_id).await?,
    };
    let base_revision_id = match input_id {
        Some(input_id) => {
            store
                .anchor_input(&conversation_id, &input_id, proposed_base_revision_id)
                .await?
        }
        None => proposed_base_revision_id,
    };
    let initial_messages = match (turn_user.as_ref(), event_id) {
        (Some(user), Some(event_id)) => vec![
            runtime::compile(
                event_id,
                mode,
                user,
                &request_context,
                &action_context,
                compiler,
                blob_sync,
            )
            .await?,
        ],
        (None, None) => Vec::new(),
        _ => {
            return Err(Error::Protocol(
                "Cursor action has an incomplete runtime event".into(),
            ))
        }
    };
    let action = if starts_turn {
        RunAction::Start
    } else {
        let pending_tool_round = match request
            .conversation_state
            .as_ref()
            .map(|state| state.pending_tool_calls.as_slice())
            .unwrap_or_default()
        {
            [] => None,
            [pending] => Some(projection::decode_pending(pending)?),
            pending => {
                return Err(Error::Protocol(format!(
                    "Cursor resume contains {} pending assistant messages",
                    pending.len()
                )))
            }
        };
        RunAction::Resume { pending_tool_round }
    };
    let kind = match (request.subagent_type_name.as_deref(), parent) {
        (None, _) => RunKind::Root,
        (Some(name), Some((parent_run_id, parent_tool_call_id))) => RunKind::Subagent {
            parent_run_id,
            parent_tool_call_id,
            kind: model::subagent_kind(name),
            background: false,
        },
        (Some(_), None) => {
            return Err(Error::Protocol(
                "subagent Run is missing its parent Run and tool call".into(),
            ));
        }
    };
    let exec = exec_context(request, &request_context, &conversation_id, &model.model_id);
    Ok((
        PreparedRun {
            run_id,
            conversation_id,
            kind,
            model,
            prompt,
            selected_subagent_models: model::selected_models(request)?,
            subagent_model_overrides: model::overrides(request)?,
            initial_messages,
            action,
            base_revision_id,
        },
        CursorRunContext {
            request_id: request_id.into(),
            mode: mode_number,
            turn_user,
            exec,
            dynamic_tools: dynamic
                .into_iter()
                .map(|(name, (wire, _))| (name, wire))
                .collect(),
        },
    ))
}

fn validate_prompt_root(messages: &[CanonicalMessage]) -> Result<()> {
    let prompts = messages
        .iter()
        .filter(|message| message.role == Role::System && message.origin == Origin::Prompt)
        .collect::<Vec<_>>();
    let [prompt] = prompts.as_slice() else {
        return Err(Error::Protocol(format!(
            "Cursor history contains {} system prompt roots",
            prompts.len()
        )));
    };
    let MessageContent::Parts { parts } = &prompt.content else {
        return Err(Error::Protocol(
            "Cursor system prompt root is not textual content".into(),
        ));
    };
    let [ContentPart::Text { .. }] = parts.as_slice() else {
        return Err(Error::Protocol(
            "Cursor system prompt root is not one text part".into(),
        ));
    };
    Ok(())
}

fn action(request_id: &str, request: &pb::AgentRunRequest) -> Result<ActionProjection> {
    let mode = request
        .conversation_state
        .as_ref()
        .and_then(|state| state.mode)
        .unwrap_or(pb::AgentMode::Agent as i32);
    let Some(action) = request
        .action
        .as_ref()
        .and_then(|action| action.action.as_ref())
    else {
        return Ok(ActionProjection {
            mode,
            turn_user: None,
            action_context: String::new(),
            event_id: None,
            input_id: None,
            starts_turn: false,
        });
    };
    match action {
        pb::conversation_action::Action::UserMessageAction(action) => {
            let user = action.user_message.as_ref().ok_or_else(|| {
                Error::Protocol("Cursor user message action has no UserMessage".into())
            })?;
            if user.message_id.is_empty() {
                return Err(Error::Protocol(
                    "Cursor user message action has no message_id".into(),
                ));
            }
            let mut context = action
                .prepend_user_messages
                .iter()
                .map(|message| message.text.trim())
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            context.extend(
                user.subagent_system_reminder
                    .iter()
                    .filter(|text| !text.is_empty())
                    .cloned(),
            );
            Ok(ActionProjection {
                mode: user.mode,
                turn_user: Some(user.clone()),
                action_context: context.join("\n\n"),
                event_id: Some(format!("run-request:{request_id}")),
                input_id: Some(format!("cursor:user:{}", user.message_id)),
                starts_turn: true,
            })
        }
        pb::conversation_action::Action::BackgroundTaskCompletionAction(action) => {
            let projection = background::project(action, mode)?;
            Ok(ActionProjection {
                mode,
                action_context: projection.context,
                event_id: Some(format!("run-request:{request_id}")),
                input_id: None,
                turn_user: Some(projection.turn_user),
                starts_turn: true,
            })
        }
        _ => Ok(ActionProjection {
            mode,
            turn_user: None,
            action_context: String::new(),
            event_id: None,
            input_id: None,
            starts_turn: false,
        }),
    }
}

fn mode_from_proto(mode: i32) -> Result<Mode> {
    let mode = pb::AgentMode::try_from(mode)
        .map_err(|_| Error::Protocol(format!("unknown Cursor agent mode: {mode}")))?;
    match mode {
        pb::AgentMode::Agent => Ok(Mode::Agent),
        pb::AgentMode::Ask => Ok(Mode::Ask),
        pb::AgentMode::Plan => Ok(Mode::Plan),
        pb::AgentMode::Debug => Ok(Mode::Debug),
        pb::AgentMode::Multitask => Ok(Mode::Multitask),
        mode => Err(Error::Protocol(format!(
            "unsupported Cursor agent mode: {}",
            mode.as_str_name()
        ))),
    }
}

fn exec_context(
    request: &pb::AgentRunRequest,
    request_context: &pb::RequestContext,
    conversation_id: &ConversationId,
    model_id: &str,
) -> ExecContext {
    let subagent_models = request
        .subagent_model_overrides
        .iter()
        .filter_map(|value| {
            use pb::subagent_model_override::Selection;
            let selection = match value.selection.as_ref()? {
                Selection::Model(model) => SubagentModel::Model(model.model_id.clone()),
                Selection::Inherit(true) => SubagentModel::Model(model_id.into()),
                Selection::Disabled(true) => SubagentModel::Disabled,
                Selection::Inherit(false) | Selection::Disabled(false) => return None,
            };
            Some((value.subagent_type.clone(), selection))
        })
        .collect();
    ExecContext {
        conversation_id: conversation_id.to_string(),
        root_conversation_id: request
            .conversation_group_id
            .clone()
            .unwrap_or_else(|| conversation_id.to_string()),
        model_id: model_id.into(),
        subagent_models,
        terminals_folder: request_context
            .env
            .as_ref()
            .map(|env| env.terminals_folder.clone())
            .unwrap_or_default(),
        admin_command_denylist: request_context.admin_command_denylist.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_system_root_is_structural_not_bound_to_the_next_model() {
        let prompt = CanonicalMessage::text(
            "root",
            Role::System,
            Origin::Prompt,
            "prompt from the previous model",
        );
        validate_prompt_root(std::slice::from_ref(&prompt)).unwrap();
        assert!(validate_prompt_root(&[prompt.clone(), prompt]).is_err());
    }

    #[test]
    fn unsupported_cursor_mode_is_not_silently_treated_as_agent() {
        assert_eq!(
            mode_from_proto(pb::AgentMode::Agent as i32).unwrap(),
            Mode::Agent
        );
        assert!(mode_from_proto(pb::AgentMode::Project as i32).is_err());
        assert!(mode_from_proto(99).is_err());
    }

    #[test]
    fn current_user_message_consumes_the_mode_instead_of_history_mode() {
        let request = pb::AgentRunRequest {
            conversation_state: Some(pb::ConversationStateStructure {
                mode: Some(pb::AgentMode::Agent as i32),
                ..Default::default()
            }),
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::UserMessageAction(
                    pb::UserMessageAction {
                        user_message: Some(pb::UserMessage {
                            text: "explain".into(),
                            message_id: "user-message".into(),
                            mode: pb::AgentMode::Ask as i32,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };
        let projection = action("request", &request).unwrap();
        assert_eq!(projection.mode, pb::AgentMode::Ask as i32);
        assert_eq!(
            projection.input_id.as_deref(),
            Some("cursor:user:user-message")
        );
        assert_eq!(mode_from_proto(projection.mode).unwrap(), Mode::Ask);
    }
}
