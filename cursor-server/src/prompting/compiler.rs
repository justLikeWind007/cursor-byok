use serde_json::Value;

use crate::{model::CanonicalMessage, Result};

use super::{project_messages, Mode, PromptAssets, ToolDefinition};

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderMessage {
    pub role: String,
    pub content: Value,
    pub thinking: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: String,
    pub model_call_id: String,
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone)]
pub struct PromptCompiler {
    assets: PromptAssets,
}

impl PromptCompiler {
    pub fn new(assets: PromptAssets) -> Self {
        Self { assets }
    }

    pub fn compile(
        &self,
        mode: Mode,
        model: impl Into<String>,
        model_call_id: impl Into<String>,
        messages: &[CanonicalMessage],
    ) -> Result<ModelRequest> {
        self.compile_with_dynamic_tools(mode, model, model_call_id, messages, &[])
    }

    pub fn compile_with_dynamic_tools(
        &self,
        mode: Mode,
        model: impl Into<String>,
        model_call_id: impl Into<String>,
        messages: &[CanonicalMessage],
        dynamic_tools: &[ToolDefinition],
    ) -> Result<ModelRequest> {
        let assets = self.assets.mode(mode);
        let mut projected = Vec::with_capacity(messages.len() + 1);
        projected.push(ProviderMessage {
            role: "system".into(),
            content: Value::String(assets.prompt.clone()),
            thinking: None,
            tool_call_id: None,
            tool_calls: None,
        });
        projected.extend(project_messages(messages)?);
        let mut tools = assets.tools.clone();
        let mut dynamic_tools = dynamic_tools.to_vec();
        dynamic_tools.sort_by(|left, right| left.name.cmp(&right.name));
        for tool in dynamic_tools {
            if let Some(existing) = tools.iter_mut().find(|existing| existing.name == tool.name) {
                *existing = tool;
            } else {
                tools.push(tool);
            }
        }
        Ok(ModelRequest {
            model: model.into(),
            model_call_id: model_call_id.into(),
            messages: projected,
            tools,
        })
    }
}
