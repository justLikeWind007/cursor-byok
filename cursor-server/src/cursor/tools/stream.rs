use crate::{
    cursor::{
        interaction,
        json_stream::{JsonStringFields, StringFieldEvent},
        proto::agent::v1 as pb,
    },
    model::ToolCall,
    Result,
};

pub struct ToolCallStream {
    presentation: Presentation,
}

enum Presentation {
    Plain,
    Edit(EditProjection),
}

struct EditProjection {
    fields: JsonStringFields,
    path_field: &'static str,
    content_field: &'static str,
    path: String,
    content: NewlineStream,
}

impl ToolCallStream {
    pub fn new(name: &str) -> Self {
        let presentation = match normalized(name).as_str() {
            "write" => Presentation::Edit(EditProjection::new("path", "contents")),
            "strreplace" => Presentation::Edit(EditProjection::new("path", "new_string")),
            "editnotebook" => {
                Presentation::Edit(EditProjection::new("target_notebook", "new_string"))
            }
            _ => Presentation::Plain,
        };
        Self { presentation }
    }

    pub fn arguments_delta(
        &mut self,
        call: &ToolCall,
        raw_delta: &str,
    ) -> Result<Vec<pb::AgentServerMessage>> {
        match &mut self.presentation {
            Presentation::Plain => Ok(vec![interaction::arguments_delta(call, raw_delta)?]),
            Presentation::Edit(edit) => {
                let mut messages = Vec::new();
                edit.project(call, raw_delta, &mut messages)?;
                Ok(messages)
            }
        }
    }
}

impl EditProjection {
    fn new(path_field: &'static str, content_field: &'static str) -> Self {
        Self {
            fields: JsonStringFields::default(),
            path_field,
            content_field,
            path: String::new(),
            content: NewlineStream::default(),
        }
    }

    fn project(
        &mut self,
        call: &ToolCall,
        raw_delta: &str,
        messages: &mut Vec<pb::AgentServerMessage>,
    ) -> Result<()> {
        for event in self.fields.push(raw_delta)? {
            match event {
                StringFieldEvent::Delta { name, text } if name == self.path_field => {
                    self.path.push_str(&text)
                }
                StringFieldEvent::End { name } if name == self.path_field => {
                    messages.push(interaction::edit_path_partial(call, &self.path));
                }
                StringFieldEvent::Delta { name, text } if name == self.content_field => {
                    let content = self.content.push(&text, false);
                    if !content.is_empty() {
                        messages.push(interaction::edit_content_delta(call, content));
                    }
                }
                StringFieldEvent::End { name } if name == self.content_field => {
                    let content = self.content.push("", true);
                    if !content.is_empty() {
                        messages.push(interaction::edit_content_delta(call, content));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct NewlineStream {
    pending_cr: bool,
}

impl NewlineStream {
    fn push(&mut self, text: &str, finished: bool) -> String {
        let mut output = String::with_capacity(text.len());
        for character in text.chars() {
            if self.pending_cr {
                output.push('\n');
                self.pending_cr = false;
                if character == '\n' {
                    continue;
                }
            }
            if character == '\r' {
                self.pending_cr = true;
            } else {
                output.push(character);
            }
        }
        if finished && self.pending_cr {
            output.push('\n');
            self.pending_cr = false;
        }
        output
    }
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            index: 0,
            call_id: "call-1".into(),
            model_call_id: "model-1".into(),
            name: name.into(),
            arguments_text: String::new(),
            arguments: Value::Null,
        }
    }

    #[test]
    fn plain_tools_only_project_raw_argument_deltas() {
        let call = call("Read");
        let mut stream = ToolCallStream::new(&call.name);
        assert_eq!(
            stream.arguments_delta(&call, "{\"path\":").unwrap().len(),
            1
        );
    }

    #[test]
    fn write_projects_path_and_content_without_starting_execution() {
        let call = call("Write");
        let mut stream = ToolCallStream::new(&call.name);
        let first = stream
            .arguments_delta(&call, "{\"path\":\"/tmp/a\",\"contents\":\"hel")
            .unwrap();
        assert_eq!(first.len(), 2);
        assert!(matches!(
            first[0].message,
            Some(pb::agent_server_message::Message::InteractionUpdate(
                pb::InteractionUpdate {
                    message: Some(pb::interaction_update::Message::PartialToolCall(_))
                }
            ))
        ));
        assert_eq!(edit_delta(&first[1]), "hel");

        let second = stream.arguments_delta(&call, "lo\\n世界\"}").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(edit_delta(&second[0]), "lo\n世界");
    }

    #[test]
    fn str_replace_projects_only_new_string_when_path_arrives_later() {
        let mut call = call("StrReplace");
        let mut stream = ToolCallStream::new(&call.name);
        let first = stream
            .arguments_delta(&call, "{\"new_string\":\"new\",\"old_string\":\"old\",")
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(edit_delta(&first[0]), "new");
        let second = stream
            .arguments_delta(&call, "\"path\":\"/tmp/a\"}")
            .unwrap();
        assert_eq!(second.len(), 1);
        assert!(matches!(
            second[0].message,
            Some(pb::agent_server_message::Message::InteractionUpdate(
                pb::InteractionUpdate {
                    message: Some(pb::interaction_update::Message::PartialToolCall(_))
                }
            ))
        ));

        call.arguments = json!({
            "path": "/tmp/a",
            "old_string": "old",
            "new_string": "new"
        });
        let rendered = interaction::render_tool_call(&call, false).unwrap();
        let Some(pb::tool_call::Tool::EditToolCall(edit)) = rendered.tool else {
            panic!("expected EditToolCall")
        };
        assert_eq!(edit.args.unwrap().stream_content.as_deref(), Some("new"));
    }

    #[test]
    fn edit_stream_normalizes_split_crlf_once() {
        let call = call("Write");
        let mut stream = ToolCallStream::new(&call.name);
        let first = stream
            .arguments_delta(&call, "{\"contents\":\"a\\r")
            .unwrap();
        let second = stream
            .arguments_delta(&call, "\\nb\\r\",\"path\":\"/tmp/a\"}")
            .unwrap();
        assert_eq!(edit_delta(&first[0]), "a");
        assert_eq!(edit_delta(&second[0]), "\nb");
        assert_eq!(edit_delta(&second[1]), "\n");
    }

    fn edit_delta(message: &pb::AgentServerMessage) -> &str {
        let Some(pb::agent_server_message::Message::InteractionUpdate(update)) = &message.message
        else {
            panic!("expected InteractionUpdate")
        };
        let Some(pb::interaction_update::Message::ToolCallDelta(update)) = &update.message else {
            panic!("expected ToolCallDelta")
        };
        let Some(pb::tool_call_delta::Delta::EditToolCallDelta(delta)) = update
            .tool_call_delta
            .as_deref()
            .and_then(|delta| delta.delta.as_ref())
        else {
            panic!("expected EditToolCallDelta")
        };
        &delta.stream_content_delta
    }
}
