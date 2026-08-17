use serde::{Deserialize, Serialize};

use super::{CanonicalMessage, ContentPart, MessageContent, Origin, Role};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub event_id: String,
    pub text: String,
}

impl RuntimeEvent {
    pub fn into_message(self) -> CanonicalMessage {
        CanonicalMessage {
            message_id: format!("runtime:{}", self.event_id),
            role: Role::User,
            origin: Origin::Runtime,
            content: MessageContent::Parts {
                parts: vec![ContentPart::Text { text: self.text }],
            },
            runtime_event_id: Some(self.event_id),
        }
    }
}
