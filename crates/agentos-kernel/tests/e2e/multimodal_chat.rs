//! Multimodal user turns: image parts reach the LLM context snapshot (mock).

use crate::common;
use agentos_kernel::Kernel;
use agentos_llm::{
    media::anthropic_blocks_for_entry, ImageResolver, MockLLMCore, MockResponse, NoopImageResolver,
};
use agentos_types::{
    AgentProfile, AgentStatus, ContentPart, ContextRole, ImageSource, LLMProvider, PermissionSet,
    ThinkingLevel,
};
use serial_test::serial;
use std::sync::Arc;

async fn register_vision_mock_agent(
    kernel: &Kernel,
    name: &str,
    responses: Vec<MockResponse>,
) -> Arc<MockLLMCore> {
    let agent_id = agentos_types::AgentID::new();
    let now = chrono::Utc::now();
    let profile = AgentProfile {
        id: agent_id,
        name: name.to_string(),
        provider: LLMProvider::Ollama,
        model: "mock-model".to_string(),
        status: AgentStatus::Online,
        permissions: PermissionSet::new(),
        roles: vec!["base".to_string()],
        current_task: None,
        description: String::new(),
        created_at: now,
        last_active: now,
        public_key_hex: None,
        base_url: None,
        default_thinking_level: ThinkingLevel::Off,
        system_prompt: None,
        manually_offline: false,
    };
    let mock = Arc::new(MockLLMCore::with_responses(responses).enable_vision());
    kernel.agent_registry.write().await.register(profile);
    kernel
        .active_llms
        .write()
        .await
        .insert(agent_id, mock.clone() as Arc<dyn agentos_llm::LLMCore>);
    mock
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn chat_inference_sees_image_part_in_mock_snapshot() {
    let (kernel, _client, _tmp, handle) = common::setup_kernel().await;
    let mock = register_vision_mock_agent(
        &kernel,
        "vision-agent",
        vec![MockResponse::text("I see a pixel.")],
    )
    .await;

    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let parts = vec![
        ContentPart::Text {
            text: "what is this?".into(),
        },
        ContentPart::Image {
            mime: "image/png".into(),
            source: ImageSource::Base64 {
                data: png_b64.into(),
            },
        },
    ];

    let result = kernel
        .chat_infer_with_tools("vision-agent", &[], "what is this?", Some(parts), None)
        .await
        .expect("chat with image");

    assert_eq!(result.answer, "I see a pixel.");

    let hist = mock.call_history();
    let snap = &hist[0].active_snapshot;
    let user_entries: Vec<_> = snap
        .iter()
        .filter(|e| e.role == ContextRole::User)
        .collect();
    let last_user = *user_entries.last().expect("user turn");
    assert!(
        last_user.has_images(),
        "expected image in last user entry, parts={:?}",
        last_user.parts
    );

    let resolver: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
    let blocks = anthropic_blocks_for_entry(last_user, true, &resolver).expect("anthropic blocks");
    let json = serde_json::Value::Array(blocks);
    let s = json.to_string();
    assert!(
        s.contains("\"type\":\"image\""),
        "anthropic payload should include image block: {s}"
    );

    kernel.shutdown();
    handle.await.unwrap();
}
