use crate::model::RuntimeEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientCommand {
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    RuntimeEvent(RuntimeEvent),
    ClientClosed {
        error: String,
    },
    Cancel,
}
