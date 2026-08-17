use std::collections::BTreeMap;

use chrono::{Offset, Utc};
use chrono_tz::Tz;

use crate::{
    cursor::{
        blob_sync::BlobSynchronizer,
        prompting::{Mode, PromptCompiler},
        proto::agent::v1 as pb,
    },
    model::{CanonicalMessage, MessageContent, Origin, Role},
    Error, Result,
};

use super::{context, images};

pub async fn compile(
    event_id: String,
    mode: Mode,
    user: &pb::UserMessage,
    request_context: &pb::RequestContext,
    action_context: &str,
    compiler: &PromptCompiler,
    blobs: &BlobSynchronizer,
) -> Result<CanonicalMessage> {
    let time = Time::now(
        request_context
            .env
            .as_ref()
            .map(|env| env.time_zone.as_str()),
    )?;
    let mut values = BTreeMap::from([
        (
            "REQUEST_CONTEXT",
            section(context::compile_context(request_context, &time.today)),
        ),
        ("OPEN_FILES", section(open_files(user))),
        (
            "SELECTED_CONTEXT",
            section(
                context::selected_context(user)
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("<selected_context>\n{value}\n</selected_context>"))
                    .unwrap_or_default(),
            ),
        ),
        ("ACTION_CONTEXT", section(action_context.to_string())),
        ("TIMESTAMP", time.timestamp),
        ("USER_QUERY", user.text.clone()),
        ("DEBUG_SERVER_ENDPOINT", String::new()),
        ("DEBUG_LOG_PATH", String::new()),
        ("DEBUG_SESSION_ID", String::new()),
    ]);
    if let Some(debug) = &request_context.debug_mode_config {
        values.insert("DEBUG_SERVER_ENDPOINT", debug.server_endpoint.clone());
        values.insert("DEBUG_LOG_PATH", debug.log_path.clone());
        values.insert("DEBUG_SESSION_ID", debug.session_id.clone());
    }
    let text = compiler.runtime_message(mode, &values)?;
    Ok(CanonicalMessage {
        message_id: format!("runtime:{event_id}"),
        role: Role::User,
        origin: Origin::Runtime,
        content: MessageContent::Parts {
            parts: images::parts(user, text, blobs).await?,
        },
        runtime_event_id: Some(event_id),
    })
}

fn section(value: String) -> String {
    let value = value.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!("{value}\n\n")
    }
}

fn open_files(user: &pb::UserMessage) -> String {
    let Some(ide) = user
        .selected_context
        .as_ref()
        .and_then(|selected| selected.invocation_context.as_ref())
        .and_then(|invocation| invocation.data.as_ref())
        .and_then(|data| match data {
            pb::invocation_context::Data::IdeState(ide) => Some(ide),
            _ => None,
        })
    else {
        return String::new();
    };
    if ide.visible_files.is_empty() && ide.recently_viewed_files.is_empty() {
        return String::new();
    }

    let mut output = String::from("<open_and_recently_viewed_files>\n");
    if !ide.recently_viewed_files.is_empty() {
        output.push_str("Recently viewed files (recent at the top, oldest at the bottom):\n");
        for file in &ide.recently_viewed_files {
            output.push_str(&format!(
                "- {} (total lines: {})\n",
                file.path, file.total_lines
            ));
        }
        output.push('\n');
    }
    if !ide.visible_files.is_empty() {
        output.push_str("Files that are currently open and visible in the user's IDE:\n");
        for (index, file) in ide.visible_files.iter().enumerate() {
            output.push_str(&format!("- {} (", file.path));
            if index == 0 {
                output.push_str("currently focused file");
                if let Some(cursor) = &file.cursor_position {
                    output.push_str(&format!(", cursor is on line {}", cursor.line));
                }
                output.push_str(&format!(", total lines: {}", file.total_lines));
            } else {
                output.push_str(&format!("total lines: {}", file.total_lines));
            }
            output.push_str(")\n");
        }
        output.push('\n');
    }
    output.push_str(
        "Note: these files may or may not be relevant to the current conversation. Use the read file tool if you need to get the contents of some of them.\n</open_and_recently_viewed_files>",
    );
    output
}

struct Time {
    timestamp: String,
    today: String,
}

impl Time {
    fn now(time_zone: Option<&str>) -> Result<Self> {
        let zone = match time_zone.filter(|value| !value.is_empty()) {
            Some(value) => value
                .parse::<Tz>()
                .map_err(|_| Error::Protocol(format!("invalid Cursor time zone: {value}")))?,
            None => chrono_tz::UTC,
        };
        let now = Utc::now().with_timezone(&zone);
        let offset = now.offset().fix().local_minus_utc();
        let sign = if offset < 0 { '-' } else { '+' };
        let offset = offset.unsigned_abs();
        let hours = offset / 3600;
        let minutes = (offset % 3600) / 60;
        let utc = if minutes == 0 {
            format!("UTC{sign}{hours}")
        } else {
            format!("UTC{sign}{hours}:{minutes:02}")
        };
        Ok(Self {
            timestamp: format!("{} ({utc})", now.format("%A, %b %-d, %Y, %-I:%M %p")),
            today: now.format("%A %b %-d,\n%Y").to_string(),
        })
    }
}
