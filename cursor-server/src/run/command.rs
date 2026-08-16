use crate::cursor::proto::agent::v1 as pb;

#[derive(Debug)]
pub enum RunCommand {
    Append {
        seqno: i64,
        message: Box<pb::AgentClientMessage>,
    },
    Abort,
    Finished,
}
