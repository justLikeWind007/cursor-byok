use std::time::Duration;

use crate::cursor::{proto::agent::v1 as pb, tools::result::ToolCompletion};

#[derive(Default)]
pub struct PresentationDelta {
    pub steps: Vec<pb::ConversationStep>,
    pub read_paths: Vec<String>,
}

#[derive(Default)]
pub struct Presentation {
    steps: Vec<pb::ConversationStep>,
    read_paths: Vec<String>,
    text: String,
    thinking: String,
}

impl Presentation {
    pub fn text_delta(&mut self, delta: &str) {
        self.text.push_str(delta);
    }

    pub fn finish_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.steps.push(pb::ConversationStep {
            message: Some(pb::conversation_step::Message::AssistantMessage(
                pb::AssistantMessage {
                    text: std::mem::take(&mut self.text),
                },
            )),
        });
    }

    pub fn thinking_delta(&mut self, delta: &str) {
        self.thinking.push_str(delta);
    }

    pub fn finish_thinking(&mut self, duration: Duration) {
        if self.thinking.is_empty() {
            return;
        }
        self.steps.push(pb::ConversationStep {
            message: Some(pb::conversation_step::Message::ThinkingMessage(
                pb::ThinkingMessage {
                    text: std::mem::take(&mut self.thinking),
                    duration_ms: duration.as_millis().min(u32::MAX as u128) as u32,
                },
            )),
        });
    }

    pub fn tool_completed(&mut self, completion: &ToolCompletion) {
        if let Some(pb::tool_call::Tool::ReadToolCall(read)) = &completion.tool_call().tool {
            if matches!(
                read.result
                    .as_ref()
                    .and_then(|result| result.result.as_ref()),
                Some(pb::read_tool_result::Result::Success(_))
            ) {
                if let Some(path) = read.args.as_ref().map(|args| &args.path) {
                    if !path.is_empty() && !self.read_paths.contains(path) {
                        self.read_paths.push(path.clone());
                    }
                }
            }
        }
        self.steps.push(pb::ConversationStep {
            message: Some(pb::conversation_step::Message::ToolCall(
                completion.tool_call().clone(),
            )),
        });
    }

    pub fn take(&mut self) -> PresentationDelta {
        PresentationDelta {
            steps: std::mem::take(&mut self.steps),
            read_paths: std::mem::take(&mut self.read_paths),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_step_keeps_the_measured_duration() {
        let mut presentation = Presentation::default();
        presentation.thinking_delta("reasoning");
        presentation.finish_thinking(Duration::from_millis(6_880));
        let step = presentation.take().steps.pop().unwrap();
        let Some(pb::conversation_step::Message::ThinkingMessage(thinking)) = step.message else {
            panic!("expected thinking step");
        };
        assert_eq!(thinking.text, "reasoning");
        assert_eq!(thinking.duration_ms, 6_880);
    }
}
