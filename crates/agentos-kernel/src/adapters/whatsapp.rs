//! WhatsApp Cloud API inbound parsing (webhook-driven).
//!
//! The webhook handler (in `agentos-api`) verifies the `X-Hub-Signature-256`
//! HMAC, then calls [`parse_whatsapp_inbound`] to build kernel `InboundMessage`s.
//! Media arrives as an opaque media **id** (not a URL): the raw message object
//! is preserved in `InboundMessage::raw`, and the `InboundRouter` later resolves
//! each id → temporary CDN URL via the Graph API (`enrich_whatsapp_media`) using
//! [`whatsapp_media_refs`].

use crate::notification_router::InboundMessage;
use agentos_types::{ChannelInstanceID, DeliveryChannel};
use chrono::Utc;
use serde_json::Value;

/// Media types whose payloads carry a downloadable `id`.
const MEDIA_TYPES: &[&str] = &["image", "document", "audio", "video", "voice", "sticker"];

/// A media reference extracted from an inbound WhatsApp message: the Graph media
/// `id`, the message type, the declared MIME (may be empty), and a filename.
pub struct WhatsAppMediaRef {
    pub media_id: String,
    pub kind: String,
    pub mime: String,
    pub filename: String,
}

/// Extract media references from a single WhatsApp message object (the value
/// stored in `InboundMessage::raw`). Used by `enrich_whatsapp_media`.
pub fn whatsapp_media_refs(message: &Value) -> Vec<WhatsAppMediaRef> {
    let mut out = Vec::new();
    for &t in MEDIA_TYPES {
        let obj = &message[t];
        if let Some(id) = obj["id"].as_str().filter(|s| !s.is_empty()) {
            out.push(WhatsAppMediaRef {
                media_id: id.to_string(),
                kind: t.to_string(),
                mime: obj["mime_type"].as_str().unwrap_or("").to_string(),
                filename: obj["filename"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    out
}

/// Parse a WhatsApp Cloud API webhook payload into kernel `InboundMessage`s,
/// one per user message under `entry[].changes[].value.messages[]`. Status
/// callbacks (delivery receipts) carry no `messages` array and are skipped.
pub fn parse_whatsapp_inbound(
    payload: &Value,
    channel_instance_id: ChannelInstanceID,
) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    for entry in payload["entry"].as_array().into_iter().flatten() {
        for change in entry["changes"].as_array().into_iter().flatten() {
            for m in change["value"]["messages"].as_array().into_iter().flatten() {
                let from = m["from"].as_str().unwrap_or("").to_string();
                let mtype = m["type"].as_str().unwrap_or("text");
                let mut text = m["text"]["body"].as_str().unwrap_or("").to_string();
                // Captioned media (image/document/video) carries a `caption`.
                if text.is_empty() {
                    if let Some(cap) = m[mtype]["caption"].as_str() {
                        text = cap.to_string();
                    }
                }
                let has_media = MEDIA_TYPES.contains(&mtype);
                // Neutral note for media with no caption so the agent knows one
                // arrived (the bytes are fetched + stored by the InboundRouter).
                if text.trim().is_empty() && has_media {
                    text = format!("[The user sent a {mtype}.]");
                }
                if text.trim().is_empty() && !has_media {
                    continue;
                }
                out.push(InboundMessage {
                    channel: DeliveryChannel::custom(DeliveryChannel::WHATSAPP),
                    channel_instance_id,
                    external_sender_id: from,
                    text,
                    reply_to_notification_id: None,
                    received_at: Utc::now(),
                    raw: m.clone(),
                    media_file_ids: Vec::new(),
                    pending_media: Vec::new(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cid() -> ChannelInstanceID {
        ChannelInstanceID::new()
    }

    #[test]
    fn parses_text_message() {
        let p = json!({
            "entry": [{ "changes": [{ "value": {
                "messages": [{ "from": "15551234567", "type": "text", "text": {"body": "hello"} }]
            }}]}]
        });
        let msgs = parse_whatsapp_inbound(&p, cid());
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "hello");
        assert_eq!(msgs[0].external_sender_id, "15551234567");
    }

    #[test]
    fn image_message_gets_note_and_media_ref() {
        let p = json!({
            "entry": [{ "changes": [{ "value": {
                "messages": [{
                    "from": "1555", "type": "image",
                    "image": { "id": "MEDIA123", "mime_type": "image/jpeg" }
                }]
            }}]}]
        });
        let msgs = parse_whatsapp_inbound(&p, cid());
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].text.contains("image"));
        let refs = whatsapp_media_refs(&msgs[0].raw);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].media_id, "MEDIA123");
        assert_eq!(refs[0].mime, "image/jpeg");
    }

    #[test]
    fn captioned_document_uses_caption() {
        let p = json!({
            "entry": [{ "changes": [{ "value": {
                "messages": [{
                    "from": "1555", "type": "document",
                    "document": { "id": "D1", "filename": "r.pdf", "mime_type": "application/pdf", "caption": "the report" }
                }]
            }}]}]
        });
        let msgs = parse_whatsapp_inbound(&p, cid());
        assert_eq!(msgs[0].text, "the report");
        assert_eq!(whatsapp_media_refs(&msgs[0].raw)[0].filename, "r.pdf");
    }

    #[test]
    fn status_callback_is_skipped() {
        let p = json!({
            "entry": [{ "changes": [{ "value": {
                "statuses": [{ "id": "wamid", "status": "delivered" }]
            }}]}]
        });
        assert!(parse_whatsapp_inbound(&p, cid()).is_empty());
    }
}
