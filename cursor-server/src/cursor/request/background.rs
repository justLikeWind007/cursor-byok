use std::collections::BTreeSet;

use crate::{cursor::proto::agent::v1 as pb, Error, Result};

pub(super) const FOLLOW_UP: &str = concat!(
    "Perform any necessary follow-up actions in response to the subagent completion above. ",
    "If no follow-up work is needed, no further action is required. ",
    "If you mention an agent or subagent in your response, link it with the `[Name](id)` ",
    "Don't use generic label such as `[agent]`, `[worker]`, or `[subagent]`. ",
    "For cloud subagents, when the agent has edited code, link to `[Review](bc-id#changes)`, ",
    "or, if you know the exact added and deleted line counts, `[Review +A −D](bc-id#changes)`, ",
    "replacing A and D with those counts. Never write A or D literally. ",
    "Use `[Try Live](bc-id#desktop)` only when the agent used computer use. ",
    "Don't repeat the same confirmation every time."
);

#[derive(Debug)]
pub(super) struct Projection {
    pub context: String,
    pub turn_user: pb::UserMessage,
}

pub(super) fn project(
    action: &pb::BackgroundTaskCompletionAction,
    mode: i32,
) -> Result<Projection> {
    if action.completions.is_empty() {
        return Err(Error::Protocol(
            "background task completion action contains no completion".into(),
        ));
    }

    let mut ids = BTreeSet::new();
    let mut contexts = Vec::with_capacity(action.completions.len());
    for completion in &action.completions {
        let kind = pb::BackgroundTaskKind::try_from(completion.kind).map_err(|_| {
            Error::Protocol(format!("unknown background task kind: {}", completion.kind))
        })?;
        if kind != pb::BackgroundTaskKind::Subagent {
            return Err(Error::Protocol(format!(
                "unsupported background task completion kind: {}",
                kind.as_str_name()
            )));
        }
        let reason =
            pb::BackgroundTaskCompletionReason::try_from(completion.reason).map_err(|_| {
                Error::Protocol(format!(
                    "unknown background task completion reason: {}",
                    completion.reason
                ))
            })?;
        if reason != pb::BackgroundTaskCompletionReason::TaskFinished {
            return Err(Error::Protocol(format!(
                "subagent notification is not a finished task: {}",
                reason.as_str_name()
            )));
        }
        let id = completion
            .subagent_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Error::Protocol("background subagent completion has no subagent_id".into())
            })?;
        if completion.task_id.is_empty() || completion.title.is_empty() {
            return Err(Error::Protocol(
                "background subagent completion requires task_id and title".into(),
            ));
        }
        if !ids.insert(id) {
            return Err(Error::Protocol(format!(
                "duplicate background subagent completion: {id}"
            )));
        }
        contexts.push(completion_context(completion, id)?);
    }

    let first = &action.completions[0];
    let message_id = ids.iter().copied().collect::<Vec<_>>().join(":");
    Ok(Projection {
        context: contexts.join("\n\n"),
        turn_user: pb::UserMessage {
            text: FOLLOW_UP.into(),
            message_id: format!("subagent-completed:{message_id}"),
            mode,
            is_simulated_msg: Some(true),
            simulated_msg_reason: Some(pb::SimulatedMsgReason::BackgroundTaskCompletion as i32),
            simulated_message_metadata: Some(pb::user_message::SimulatedMessageMetadata {
                title: Some(first.title.clone()),
                task_id: Some(first.task_id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    })
}

fn completion_context(completion: &pb::BackgroundTaskCompletion, id: &str) -> Result<String> {
    let status = pb::BackgroundTaskStatus::try_from(completion.status).map_err(|_| {
        Error::Protocol(format!(
            "unknown background task status: {}",
            completion.status
        ))
    })?;
    if status == pb::BackgroundTaskStatus::Unspecified {
        return Err(Error::Protocol(
            "background subagent completion has unspecified status".into(),
        ));
    }
    let mut fields = vec![
        format!("Title: {}", completion.title),
        format!("Subagent ID: {id}"),
        format!("Status: {}", status.as_str_name()),
    ];
    if let Some(tool_call_id) = completion
        .tool_call_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        fields.push(format!("Tool call ID: {tool_call_id}"));
    }
    if let Some(output_path) = completion
        .output_path
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        fields.push(format!("Output path: {output_path}"));
    }
    if let Some(detail) = completion
        .detail
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        fields.push(detail.into());
    }
    Ok(format!(
        "<background_task_completion>\n{}\n</background_task_completion>",
        fields.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_subagent_becomes_an_idempotent_user_runtime_event() {
        let action = pb::BackgroundTaskCompletionAction {
            completions: vec![completion()],
        };
        let projection = project(&action, pb::AgentMode::Multitask as i32).unwrap();

        assert!(projection.context.contains("Subagent ID: child-id"));
        assert!(projection.context.contains("child result"));

        assert_eq!(projection.turn_user.text, FOLLOW_UP);
        assert_eq!(projection.turn_user.is_simulated_msg, Some(true));
        assert_eq!(
            projection.turn_user.simulated_msg_reason,
            Some(pb::SimulatedMsgReason::BackgroundTaskCompletion as i32)
        );
    }

    #[test]
    fn completion_requires_the_captured_subagent_identity_and_terminal_reason() {
        let mut value = completion();
        value.subagent_id = None;
        assert!(project(
            &pb::BackgroundTaskCompletionAction {
                completions: vec![value]
            },
            pb::AgentMode::Agent as i32
        )
        .unwrap_err()
        .to_string()
        .contains("subagent_id"));

        let mut value = completion();
        value.reason = pb::BackgroundTaskCompletionReason::TaskProgress as i32;
        assert!(project(
            &pb::BackgroundTaskCompletionAction {
                completions: vec![value]
            },
            pb::AgentMode::Agent as i32
        )
        .unwrap_err()
        .to_string()
        .contains("not a finished task"));
    }

    fn completion() -> pb::BackgroundTaskCompletion {
        pb::BackgroundTaskCompletion {
            task_id: "child-id".into(),
            kind: pb::BackgroundTaskKind::Subagent as i32,
            status: pb::BackgroundTaskStatus::Success as i32,
            title: "Inspect protocol".into(),
            detail: Some("child result".into()),
            reason: pb::BackgroundTaskCompletionReason::TaskFinished as i32,
            subagent_id: Some("child-id".into()),
            tool_call_id: Some("task-call".into()),
            ..Default::default()
        }
    }
}
