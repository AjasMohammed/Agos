//! Golden-style checks for multimodal JSON shapes (no network).

use agentos_llm::media::{
    anthropic_blocks_for_entry, gemini_user_parts, openai_user_content_value, NoopImageResolver,
};
use agentos_llm::ImageResolver;
use agentos_types::{ContentPart, ContextEntry, ContextRole, ImageSource};
use std::sync::Arc;

fn sample_user_with_png() -> ContextEntry {
    let mut e = ContextEntry::from_text(ContextRole::User, "look");
    e.parts.push(ContentPart::Image {
        mime: "image/png".into(),
        source: ImageSource::Base64 {
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                .into(),
        },
    });
    e
}

#[test]
fn anthropic_payload_contains_image_block() {
    let e = sample_user_with_png();
    let r: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
    let blocks = anthropic_blocks_for_entry(&e, true, &r).unwrap();
    let v = serde_json::to_value(&blocks).unwrap();
    assert!(v.to_string().contains("image"), "{v}");
}

#[test]
fn openai_payload_uses_data_uri_for_image() {
    let e = sample_user_with_png();
    let r: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
    let content = openai_user_content_value(&e, true, &r);
    let s = content.to_string();
    assert!(s.contains("image_url"));
    assert!(s.contains("data:image/png;base64,"));
}

#[test]
fn gemini_payload_contains_inline_data() {
    let e = sample_user_with_png();
    let r: Arc<dyn ImageResolver> = Arc::new(NoopImageResolver);
    let parts = gemini_user_parts(&e, true, &r);
    let v = serde_json::to_value(&parts).unwrap();
    assert!(v.to_string().contains("inline_data"));
}
