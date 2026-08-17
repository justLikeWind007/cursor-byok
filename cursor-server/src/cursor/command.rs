use crate::cursor::proto::agent::v1 as pb;

#[derive(Debug)]
pub enum CursorCommand {
    Append {
        seqno: i64,
        message: Box<pb::AgentClientMessage>,
    },
    Abort,
    Finished,
}
