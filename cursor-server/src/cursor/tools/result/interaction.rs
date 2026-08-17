use crate::{
    cursor::{interaction, proto::agent::v1 as pb},
    Error, Result,
};

use super::ToolCompletion;
use crate::cursor::tools::runtime::PendingInteraction;

pub(crate) fn from_interaction(
    pending: PendingInteraction,
    response: &pb::InteractionResponse,
) -> Result<ToolCompletion> {
    use pb::{interaction_response::Result as Response, tool_call::Tool};
    let call = &pending.call;
    let mut rendered = interaction::render_tool_call(call, false)?;
    let (output, is_error) = match (rendered.tool.as_mut(), response.result.as_ref()) {
        (
            Some(Tool::AskQuestionToolCall(tool)),
            Some(Response::AskQuestionInteractionResponse(value)),
        ) => {
            let result = value
                .result
                .clone()
                .ok_or_else(|| missing("ask question"))?;
            let output = ask_output(&result)?;
            tool.result = Some(result);
            output
        }
        (
            Some(Tool::CreatePlanToolCall(tool)),
            Some(Response::CreatePlanRequestResponse(value)),
        ) => {
            let result = value.result.clone().ok_or_else(|| missing("create plan"))?;
            let output = create_plan_output(&result)?;
            tool.result = Some(result);
            output
        }
        (
            Some(Tool::SwitchModeToolCall(tool)),
            Some(Response::SwitchModeRequestResponse(value)),
        ) => {
            let (result, output) = switch_mode_result(value)?;
            tool.result = Some(result);
            output
        }
        (Some(Tool::WebSearchToolCall(tool)), Some(Response::WebSearchRequestResponse(value))) => {
            match value
                .result
                .as_ref()
                .ok_or_else(|| missing("web search approval"))?
            {
                pb::web_search_request_response::Result::Rejected(rejected) => {
                    tool.result = Some(pb::WebSearchResult {
                        result: Some(pb::web_search_result::Result::Rejected(
                            pb::WebSearchRejected {
                                reason: rejected.reason.clone(),
                            },
                        )),
                    });
                    (rejected.reason.clone(), true)
                }
                pb::web_search_request_response::Result::Approved(_) => {
                    return Err(Error::Provider(
                        "WebSearch requires a configured server-side search executor".into(),
                    ));
                }
            }
        }
        (Some(Tool::WebFetchToolCall(tool)), Some(Response::WebFetchRequestResponse(value))) => {
            match value
                .result
                .as_ref()
                .ok_or_else(|| missing("web fetch approval"))?
            {
                pb::web_fetch_request_response::Result::Rejected(rejected) => {
                    tool.result = Some(pb::WebFetchResult {
                        result: Some(pb::web_fetch_result::Result::Rejected(
                            pb::WebFetchRejected {
                                reason: rejected.reason.clone(),
                            },
                        )),
                    });
                    (rejected.reason.clone(), true)
                }
                pb::web_fetch_request_response::Result::Approved(_) => {
                    return Err(Error::Protocol(
                        "WebFetch approval is not a terminal tool result".into(),
                    ));
                }
            }
        }
        (
            Some(Tool::GenerateImageToolCall(tool)),
            Some(Response::GenerateImageRequestResponse(value)),
        ) => match value
            .result
            .as_ref()
            .ok_or_else(|| missing("generate image approval"))?
        {
            pb::generate_image_request_response::Result::Rejected(rejected) => {
                tool.result = Some(pb::GenerateImageResult {
                    result: Some(pb::generate_image_result::Result::Error(
                        pb::GenerateImageError {
                            error: rejected.reason.clone(),
                        },
                    )),
                });
                (rejected.reason.clone(), true)
            }
            pb::generate_image_request_response::Result::Approved(_) => {
                return Err(Error::Provider(
                    "GenerateImage requires a configured server-side image executor".into(),
                ));
            }
        },
        _ => {
            return Err(Error::Protocol(format!(
                "unexpected InteractionResponse for tool {}",
                call.name
            )));
        }
    };
    ToolCompletion::from_rendered(call, pending.started_at_ms, output, is_error, rendered)
}

fn ask_output(value: &pb::AskQuestionResult) -> Result<(String, bool)> {
    use pb::ask_question_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("ask question"))?
    {
        R::Success(value) => Ok((
            value
                .answers
                .iter()
                .map(|answer| {
                    let value = if answer.freeform_text.is_empty() {
                        answer.selected_option_ids.join(", ")
                    } else {
                        answer.freeform_text.clone()
                    };
                    format!("{}: {value}", answer.question_id)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        )),
        R::Error(value) => Ok((value.error_message.clone(), true)),
        R::Rejected(value) => Ok((value.reason.clone(), true)),
        R::Async(_) => Ok(("question is running asynchronously".into(), false)),
    }
}

fn create_plan_output(value: &pb::CreatePlanResult) -> Result<(String, bool)> {
    use pb::create_plan_result::Result as R;
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("create plan"))?
    {
        R::Success(_) => Ok((format!("plan created: {}", value.plan_uri), false)),
        R::Error(value) => Ok((value.error.clone(), true)),
    }
}

fn switch_mode_result(
    value: &pb::SwitchModeRequestResponse,
) -> Result<(pb::SwitchModeResult, (String, bool))> {
    use pb::{switch_mode_request_response::Result as Input, switch_mode_result::Result as Output};
    match value
        .result
        .as_ref()
        .ok_or_else(|| missing("switch mode"))?
    {
        Input::Approved(_) => Ok((
            pb::SwitchModeResult {
                result: Some(Output::Success(pb::SwitchModeSuccess::default())),
            },
            ("mode switched".into(), false),
        )),
        Input::Rejected(value) => Ok((
            pb::SwitchModeResult {
                result: Some(Output::Rejected(pb::SwitchModeRejected {
                    reason: value.reason.clone(),
                })),
            },
            (value.reason.clone(), true),
        )),
    }
}

fn missing(name: &str) -> Error {
    Error::Protocol(format!("{name} returned no result"))
}
