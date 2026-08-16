use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

static EMBEDDED_PROMPTS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../prompt");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Agent,
    Ask,
    Plan,
    Debug,
    Multitask,
    Subagent,
    Compaction,
    Commit,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "agent" => Ok(Self::Agent),
            "ask" => Ok(Self::Ask),
            "plan" => Ok(Self::Plan),
            "debug" => Ok(Self::Debug),
            "multitask" => Ok(Self::Multitask),
            "subagent" => Ok(Self::Subagent),
            "compaction" => Ok(Self::Compaction),
            "commit" => Ok(Self::Commit),
            other => Err(Error::Config(format!("unknown prompt mode: {other}"))),
        }
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Ask => "ask",
            Self::Plan => "plan",
            Self::Debug => "debug",
            Self::Multitask => "multitask",
            Self::Subagent => "subagent",
            Self::Compaction => "compaction",
            Self::Commit => "commit",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug)]
pub struct ModeAssets {
    pub prompt: String,
    pub tools: Vec<ToolDefinition>,
    pub reminders: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct PromptAssets {
    modes: HashMap<Mode, ModeAssets>,
}

impl PromptAssets {
    pub fn load(root: &Path) -> Result<Self> {
        Self::read(|directory, filename| {
            let path = root.join(directory).join(filename);
            path.exists()
                .then(|| std::fs::read_to_string(path).map_err(Error::from))
                .transpose()
        })
    }

    pub fn embedded() -> Result<Self> {
        Self::read(|directory, filename| {
            EMBEDDED_PROMPTS
                .get_file(format!("{directory}/{filename}"))
                .map(|file| {
                    file.contents_utf8().map(str::to_string).ok_or_else(|| {
                        Error::Config(format!("prompt asset is not UTF-8: {directory}/{filename}"))
                    })
                })
                .transpose()
        })
    }

    fn read(mut asset: impl FnMut(&str, &str) -> Result<Option<String>>) -> Result<Self> {
        let mut modes = HashMap::new();
        for mode in [
            Mode::Agent,
            Mode::Ask,
            Mode::Plan,
            Mode::Debug,
            Mode::Multitask,
            Mode::Subagent,
            Mode::Compaction,
            Mode::Commit,
        ] {
            let prompt = asset(mode.directory(), "prompt.md")?
                .ok_or_else(|| Error::Config(format!("missing prompt for {:?}", mode)))?;
            if prompt.trim().is_empty() {
                return Err(Error::Config(format!("empty prompt for {:?}", mode)));
            }
            let tools = if let Some(contents) = asset(mode.directory(), "tools.json")? {
                parse_tools(&contents)?
            } else {
                Vec::new()
            };
            let mut reminders = HashMap::new();
            for reminder in [
                "system_reminder.txt",
                "system_reminder_initial.txt",
                "system_reminder_continuing.txt",
            ] {
                if let Some(contents) = asset(mode.directory(), reminder)? {
                    reminders.insert(reminder.trim_end_matches(".txt").into(), contents);
                }
            }
            modes.insert(
                mode,
                ModeAssets {
                    prompt,
                    tools,
                    reminders,
                },
            );
        }
        Ok(Self { modes })
    }

    pub fn mode(&self, mode: Mode) -> &ModeAssets {
        self.modes
            .get(&mode)
            .expect("all modes validated at startup")
    }
}

fn parse_tools(json: &str) -> Result<Vec<ToolDefinition>> {
    let value: Value = serde_json::from_str(json)?;
    let array = value
        .as_array()
        .ok_or_else(|| Error::Config("tools.json must be an array".into()))?;
    array
        .iter()
        .map(|tool| {
            let source = tool
                .get("function")
                .ok_or_else(|| Error::Config("tool is missing function".into()))?;
            Ok(ToolDefinition {
                name: source
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Config("tool is missing name".into()))?
                    .into(),
                description: source
                    .get("description")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Config("tool is missing description".into()))?
                    .to_string(),
                input_schema: source
                    .get("parameters")
                    .cloned()
                    .ok_or_else(|| Error::Config("tool is missing parameters".into()))?,
            })
        })
        .collect()
}
