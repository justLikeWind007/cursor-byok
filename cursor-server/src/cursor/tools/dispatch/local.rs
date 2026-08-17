//! Synchronous local tool dispatch.

use crate::{model::ToolCall, Result};

use super::ToolStart;
use crate::cursor::tools::result;

pub(super) fn start(call: &ToolCall, message_index: usize) -> Result<ToolStart> {
    Ok(ToolStart {
        messages: Vec::new(),
        completion: Some(result::local(call, message_index)?),
    })
}
