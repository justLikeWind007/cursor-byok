#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use cursor_server::{
    cursor::{
        connect,
        prompting::{PromptAssets, PromptCompiler},
        proto::agent::v1 as pb,
        CursorCommand, CursorSessionRegistry,
    },
    model::{ContentPart, ProjectedContent},
    provider::{FinishReason, ModelEvent},
    store::{BlobId, Store},
};
use prost::Message;

#[tokio::test]
async fn current_mode_and_referenced_context_are_consumed_by_one_runtime_message() {
    let (_directory, store) = fixtures::temp_store().await;
    let references = references(&store).await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "model".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("answer".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let registry = CursorSessionRegistry::new(
        store,
        Arc::new(provider.clone()),
        PromptCompiler::new(assets),
        Default::default(),
    );
    let handle = registry.get_or_create("ask-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(CursorCommand::Append {
            seqno: 0,
            message: Box::new(run_request(references)),
        })
        .await
        .unwrap();

    let mut seqno = 1;
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), output.recv())
            .await
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        if flags & connect::END_STREAM_FLAG != 0 {
            break;
        }
        let message = pb::AgentServerMessage::decode(payload).unwrap();
        if let Some(pb::agent_server_message::Message::KvServerMessage(kv)) = message.message {
            handle
                .command(CursorCommand::Append {
                    seqno,
                    message: Box::new(kv_ack(kv.id)),
                })
                .await
                .unwrap();
            seqno += 1;
        }
    }

    let requests = provider.requests();
    let request = &requests[0];
    assert!(request
        .prompt
        .tools
        .iter()
        .any(|tool| tool.name == "AskQuestion"));
    assert!(!request
        .prompt
        .tools
        .iter()
        .any(|tool| tool.name == "GenerateImage"));
    assert_eq!(request.history.len(), 1);
    assert_eq!(
        request.history[0].message_id,
        "runtime:run-request:ask-request"
    );
    let ProjectedContent::Parts(parts) = &request.history[0].content else {
        panic!("runtime message must use typed parts")
    };
    let [ContentPart::Text { text }] = parts.as_slice() else {
        panic!("this fixture has no images")
    };
    for expected in [
        "<user_rule>workspace rule</user_rule>",
        "<agent_skill fullPath=\"/skills/test/SKILL.md\">test skill</agent_skill>",
        "<subagent name=\"reviewer\">review code</subagent>",
        "<mcp_meta_tool_server name=\"mcp-test\" tools=\"lookup\" />",
        "Ask mode is active.",
        "<user_query>\nexplain this\n</user_query>",
    ] {
        assert!(
            text.contains(expected),
            "missing runtime section: {expected}"
        );
    }
    assert!(text.contains("/workspace/src/main.rs"));
}

struct References {
    rules: (BlobId, u32),
    skills: (BlobId, u32),
    subagents: (BlobId, u32),
    mcps: (BlobId, u32),
}

async fn references(store: &Store) -> References {
    References {
        rules: put(
            store,
            &pb::RequestContextRulesPart {
                rules: vec![pb::CursorRule {
                    full_path: "/workspace/AGENTS.md".into(),
                    content: "workspace rule".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await,
        skills: put(
            store,
            &pb::RequestContextSkillsPart {
                agent_skills: vec![pb::AgentSkill {
                    full_path: "/skills/test/SKILL.md".into(),
                    description: "test skill".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await,
        subagents: put(
            store,
            &pb::RequestContextSubagentsPart {
                custom_subagents: vec![pb::CustomSubagent {
                    name: "reviewer".into(),
                    description: "review code".into(),
                    ..Default::default()
                }],
            },
        )
        .await,
        mcps: put(
            store,
            &pb::RequestContextMcpsPart {
                mcp_meta_tool_options: Some(pb::McpMetaToolOptions {
                    enabled: true,
                    mcp_descriptors: vec![pb::McpDescriptor {
                        server_identifier: "mcp-test".into(),
                        tools: vec![pb::McpToolDescriptor {
                            tool_name: "lookup".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            },
        )
        .await,
    }
}

async fn put<T: Message>(store: &Store, value: &T) -> (BlobId, u32) {
    let data = value.encode_to_vec();
    let length = data.len() as u32;
    (store.put_blob(&data, &[]).await.unwrap(), length)
}

fn run_request(references: References) -> pb::AgentClientMessage {
    let (rules, rules_byte_length) = references.rules;
    let (skills, skills_byte_length) = references.skills;
    let (subagents, subagents_byte_length) = references.subagents;
    let (mcps, mcps_byte_length) = references.mcps;
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                conversation_state: Some(pb::ConversationStateStructure {
                    mode: Some(pb::AgentMode::Agent as i32),
                    ..Default::default()
                }),
                action: Some(pb::ConversationAction {
                    request_context_parts: Some(pb::RequestContextPartReferences {
                        rules_blob_id: rules.as_bytes().to_vec(),
                        rules_byte_length,
                        skills_blob_id: skills.as_bytes().to_vec(),
                        skills_byte_length,
                        subagents_blob_id: subagents.as_bytes().to_vec(),
                        subagents_byte_length,
                        mcps_blob_id: mcps.as_bytes().to_vec(),
                        mcps_byte_length,
                        dynamic_context: Some(pb::RequestContext {
                            env: Some(pb::RequestContextEnv {
                                os_version: "darwin".into(),
                                workspace_paths: vec!["/workspace".into()],
                                shell: "zsh".into(),
                                time_zone: "UTC".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                    }),
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "explain this".into(),
                                message_id: "wire-user".into(),
                                mode: pb::AgentMode::Ask as i32,
                                selected_context: Some(pb::SelectedContext {
                                    invocation_context: Some(pb::InvocationContext {
                                        data: Some(pb::invocation_context::Data::IdeState(
                                            pb::invocation_context::IdeState {
                                                visible_files: vec![
                                                    pb::invocation_context::ide_state::File {
                                                        path: "/workspace/src/main.rs".into(),
                                                        total_lines: 10,
                                                        ..Default::default()
                                                    },
                                                ],
                                                ..Default::default()
                                            },
                                        )),
                                    }),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("mode-conversation".into()),
                run_id: Some("wire-run".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

fn kv_ack(id: u32) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::KvClientMessage(
            pb::KvClientMessage {
                id,
                message: Some(pb::kv_client_message::Message::SetBlobResult(
                    pb::SetBlobResult { error: None },
                )),
            },
        )),
    }
}
