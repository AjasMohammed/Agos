use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_http::{client, HttpProfile};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};
use zeroize::Zeroizing;

/// Build inbound message content from a Discord `MESSAGE_CREATE` `d` object,
/// combining the text body and any `attachments` (public CDN URLs). Image
/// attachments (by `content_type`) become `MessageContent::Image` so the kernel
/// feeds them to vision; others become `File`. Returns `None` when there is
/// neither text nor a usable attachment (nothing to forward).
fn discord_message_content(d: &serde_json::Value) -> Option<MessageContent> {
    let text = d["content"].as_str().unwrap_or("");
    let mut media: Vec<MessageContent> = Vec::new();
    if let Some(atts) = d["attachments"].as_array() {
        for att in atts {
            let url = match att["url"].as_str() {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => continue,
            };
            let filename = att["filename"].as_str().unwrap_or("file").to_string();
            let mime = att["content_type"].as_str().unwrap_or("");
            if mime.starts_with("image/") {
                media.push(MessageContent::Image {
                    url,
                    alt: (!filename.is_empty()).then(|| filename.clone()),
                });
            } else {
                media.push(MessageContent::File {
                    url,
                    filename,
                    mime: if mime.is_empty() {
                        "application/octet-stream".to_string()
                    } else {
                        mime.to_string()
                    },
                });
            }
        }
    }
    match (text.trim().is_empty(), media.len()) {
        (true, 0) => None,
        (false, 0) => Some(MessageContent::Text(text.to_string())),
        (true, 1) => media.into_iter().next(),
        _ => {
            let mut parts = Vec::new();
            if !text.trim().is_empty() {
                parts.push(MessageContent::Text(text.to_string()));
            }
            parts.extend(media);
            Some(MessageContent::Mixed(parts))
        }
    }
}

pub struct DiscordAdapter {
    bot_token: Zeroizing<String>,
    pub channel_id: String,
    pub instance_id: String,
    client: reqwest::Client,
    /// Set to `false` when the Gateway WS listener exits, so `health_check`
    /// reflects listener death rather than only REST reachability.
    listener_alive: Arc<AtomicBool>,
}

impl DiscordAdapter {
    pub fn new(bot_token: String, channel_id: String, instance_id: String) -> Self {
        Self {
            bot_token: Zeroizing::new(bot_token),
            channel_id,
            instance_id,
            client: client(HttpProfile::Outbound),
            listener_alive: Arc::new(AtomicBool::new(false)),
        }
    }

    fn rest_url(&self, path: &str) -> String {
        format!("https://discord.com/api/v10{}", path)
    }

    fn auth_header(&self) -> String {
        format!("Bot {}", self.bot_token.as_str())
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: true,
            media: true,
            rich_formatting: true,
            max_message_length: 2000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        // Controls render as button components, so the text fallback is only
        // used when there are none — appending it alongside the buttons would
        // show the same commands twice.
        // `render_for_delivery`, not `text_with_actions`: controls render as
        // button components below, so appending the text instructions too
        // would show the same commands twice.
        let text: String = msg
            .content
            .render_for_delivery()
            .chars()
            .take(2000)
            .collect();
        let url = self.rest_url(&format!("/channels/{}/messages", self.channel_id));
        let client = &self.client;
        let auth = self.auth_header();
        let policy = crate::retry::RetryPolicy::default();
        let components = message_components(&msg.actions);

        crate::retry::with_retry(&policy, "discord", || async {
            let mut payload = serde_json::json!({"content": &text});
            if !components.is_null() {
                payload["components"] = components.clone();
            }
            let resp = client
                .post(&url)
                .header("Authorization", &auth)
                .json(&payload)
                .send()
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "discord".to_string(),
                    reason: e.to_string(),
                })?;
            if !resp.status().is_success() {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "discord".to_string(),
                    reason: format!("Discord API error: {}", resp.status()),
                });
            }
            Ok(DeliveryReceipt {
                message_id: uuid::Uuid::new_v4().to_string(),
                delivered_at: chrono::Utc::now(),
            })
        })
        .await
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        let gateway_url = "wss://gateway.discord.gg/?v=10&encoding=json";
        let instance_id = self.instance_id.clone();
        let channel_id = self.channel_id.clone();
        let listener_alive = self.listener_alive.clone();
        // Needed to acknowledge component interactions: Discord shows "This
        // interaction failed" unless the callback endpoint is hit within 3s.
        let http = self.client.clone();
        let auth = self.auth_header();

        let mut reconnect_delay = std::time::Duration::from_secs(1);
        let max_reconnect_delay = std::time::Duration::from_secs(60);

        // Outer reconnect loop — re-establishes the WebSocket on disconnect.
        loop {
            if cancel.is_cancelled() {
                break;
            }

            let connect_result = tokio_tungstenite::connect_async(gateway_url).await;
            let ws_stream = match connect_result {
                Ok((ws, _)) => {
                    reconnect_delay = std::time::Duration::from_secs(1); // reset on success
                    ws
                }
                Err(e) => {
                    error!(
                        "Discord WS connect failed: {e}, retrying in {:?}",
                        reconnect_delay
                    );
                    listener_alive.store(false, Ordering::Release);
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(reconnect_delay) => {},
                    }
                    reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
                    continue;
                }
            };

            let (mut write, mut read) = ws_stream.split();
            let mut heartbeat_interval: Option<tokio::time::Interval> = None;
            let mut sequence: Option<u64> = None;
            // Fresh token for each connection attempt.
            let mut bot_token: Option<zeroize::Zeroizing<String>> = Some(self.bot_token.clone());

            // Inner message loop — processes messages until disconnect.
            loop {
                if cancel.is_cancelled() {
                    break;
                }

                let tick = async {
                    if let Some(ref mut interval) = heartbeat_interval {
                        interval.tick().await;
                        true
                    } else {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        false
                    }
                };

                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
                                    let op = payload["op"].as_u64().unwrap_or(255);

                                    match op {
                                        10 => { // HELLO
                                            let interval_ms = payload["d"]["heartbeat_interval"]
                                                .as_u64()
                                                .unwrap_or(41250);
                                            // Per Discord docs: jitter the first heartbeat to avoid
                                            // thundering herd on reconnect.
                                            let jitter_ms = rand::thread_rng().gen_range(0..interval_ms);
                                            heartbeat_interval = Some(tokio::time::interval_at(
                                                tokio::time::Instant::now()
                                                    + std::time::Duration::from_millis(jitter_ms),
                                                std::time::Duration::from_millis(interval_ms),
                                            ));
                                            if let Some(token) = bot_token.take() {
                                                // GUILD_MESSAGES(512) | MESSAGE_CONTENT(32768) = 33280
                                                let identify = serde_json::json!({
                                                    "op": 2,
                                                    "d": {
                                                        "token": token.as_str(),
                                                        "intents": 33280,
                                                        "properties": {
                                                            "os": "linux",
                                                            "browser": "agentos",
                                                            "device": "agentos"
                                                        }
                                                    }
                                                });
                                                let _ = write.send(Message::Text(identify.to_string())).await;
                                            }
                                        }
                                        0 => { // DISPATCH — only DISPATCH carries meaningful sequence numbers
                                            // Update sequence cursor only on DISPATCH events.
                                            if let Some(seq) = payload["s"].as_u64() {
                                                sequence = Some(seq);
                                            }
                                            let event_type = payload["t"].as_str();
                                            if event_type == Some("READY") {
                                                // Gateway is ready and authenticated.
                                                listener_alive.store(true, Ordering::Release);
                                            }
                                            // A button press arrives here, not as a message.
                                            // `custom_id` is the literal command, so it feeds the
                                            // same inbound path as typed text.
                                            if event_type == Some("INTERACTION_CREATE") {
                                                let d = &payload["d"];
                                                if let Some(inbound) = interaction_inbound(
                                                    d,
                                                    &channel_id,
                                                    &instance_id,
                                                    &payload,
                                                ) {
                                                    // Ack first, and never block on it: the 3s
                                                    // budget is Discord's, and the escalation's own
                                                    // reply comes back through the normal outbound
                                                    // path. Type 6 = DEFERRED_UPDATE_MESSAGE, which
                                                    // clears the spinner and changes nothing.
                                                    if let (Some(id), Some(token)) =
                                                        (d["id"].as_str(), d["token"].as_str())
                                                    {
                                                        let ack_url = format!(
                                                            "https://discord.com/api/v10/interactions/{id}/{token}/callback"
                                                        );
                                                        let http = http.clone();
                                                        let auth = auth.clone();
                                                        tokio::spawn(async move {
                                                            if let Err(e) = http
                                                                .post(&ack_url)
                                                                .header("Authorization", &auth)
                                                                .json(&serde_json::json!({"type": 6}))
                                                                .send()
                                                                .await
                                                            {
                                                                warn!("Discord interaction ack failed: {e}");
                                                            }
                                                        });
                                                    }
                                                    let _ = tx.send(inbound).await;
                                                }
                                            }
                                            if event_type == Some("MESSAGE_CREATE") {
                                                let d = &payload["d"];
                                                // Skip bot/self-authored messages so the bot's own
                                                // posts don't echo back as inbound (loop guard).
                                                if d["channel_id"].as_str() == Some(&channel_id)
                                                    && d["author"]["bot"].as_bool() != Some(true)
                                                {
                                                    // Combine text + attachments (images → vision, files → note).
                                                    if let Some(content) = discord_message_content(d) {
                                                        let inbound = InboundMessage {
                                                            id: d["id"].as_str().unwrap_or("").to_string(),
                                                            channel_type: "discord".to_string(),
                                                            channel_instance_id: instance_id.clone(),
                                                            sender: ChannelIdentity {
                                                                platform_id: d["author"]["id"]
                                                                    .as_str()
                                                                    .unwrap_or("")
                                                                    .to_string(),
                                                                display_name: d["author"]["username"]
                                                                    .as_str()
                                                                    .map(String::from),
                                                            },
                                                            content,
                                                            thread_id: None,
                                                            timestamp: chrono::Utc::now(),
                                                            raw: payload.clone(),
                                                        };
                                                        let _ = tx.send(inbound).await;
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                error!("Discord WS error: {e}, will reconnect");
                                break; // break inner loop to reconnect
                            }
                            None => break, // stream ended, reconnect
                            _ => {}
                        }
                    }
                    should_heartbeat = tick => {
                        if should_heartbeat {
                            let hb = serde_json::json!({"op": 1, "d": sequence});
                            let _ = write.send(Message::Text(hb.to_string())).await;
                        }
                    }
                }
            }

            listener_alive.store(false, Ordering::Release);

            // If cancelled, exit cleanly
            if cancel.is_cancelled() {
                break;
            }

            // Backoff before reconnect
            tracing::warn!(
                "Discord listener disconnected, reconnecting in {:?}",
                reconnect_delay
            );
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(reconnect_delay) => {},
            }
            reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
        }
        listener_alive.store(false, Ordering::Release);
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        // If the listener has started but is no longer alive, report degraded
        // regardless of REST reachability — inbound messages are not flowing.
        if !self.listener_alive.load(Ordering::Acquire) {
            // Listener not yet started (false on new()) or has exited.
            // Fall through to REST check; caller can infer listener state from logs.
        }

        let url = self.rest_url("/users/@me");
        match self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                if self.listener_alive.load(Ordering::Acquire) {
                    ChannelHealth::Connected
                } else {
                    ChannelHealth::Degraded(
                        "REST reachable but Gateway listener is not running".to_string(),
                    )
                }
            }
            Ok(r) => ChannelHealth::Degraded(format!("status {}", r.status())),
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_only_message() {
        let d = json!({ "content": "hello", "attachments": [] });
        assert!(matches!(
            discord_message_content(&d),
            Some(MessageContent::Text(t)) if t == "hello"
        ));
    }

    #[test]
    fn empty_message_is_none() {
        let d = json!({ "content": "", "attachments": [] });
        assert!(discord_message_content(&d).is_none());
    }

    #[test]
    fn image_only_becomes_image() {
        let d = json!({
            "content": "",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/cat.png", "filename": "cat.png", "content_type": "image/png" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Image { url, alt }) => {
                assert_eq!(url, "https://cdn.discordapp.com/a/cat.png");
                assert_eq!(alt.as_deref(), Some("cat.png"));
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn text_plus_image_becomes_mixed() {
        let d = json!({
            "content": "look",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/x.jpg", "filename": "x.jpg", "content_type": "image/jpeg" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Mixed(parts)) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "look"));
                assert!(matches!(&parts[1], MessageContent::Image { .. }));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn non_image_becomes_file_with_default_mime() {
        let d = json!({
            "content": "",
            "attachments": [
                { "url": "https://cdn.discordapp.com/a/report", "filename": "report.pdf" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::File { mime, filename, .. }) => {
                assert_eq!(filename, "report.pdf");
                assert_eq!(mime, "application/octet-stream");
            }
            other => panic!("expected File, got {other:?}"),
        }
    }

    #[test]
    fn attachment_without_url_is_skipped() {
        let d = json!({ "content": "", "attachments": [ { "filename": "x" } ] });
        assert!(discord_message_content(&d).is_none());
    }

    #[test]
    fn text_plus_multiple_attachments_keeps_order() {
        let d = json!({
            "content": "hi",
            "attachments": [
                { "url": "https://cdn/x.png", "filename": "x.png", "content_type": "image/png" },
                { "url": "https://cdn/y.pdf", "filename": "y.pdf", "content_type": "application/pdf" }
            ]
        });
        match discord_message_content(&d) {
            Some(MessageContent::Mixed(parts)) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(&parts[0], MessageContent::Text(t) if t == "hi"));
                assert!(matches!(&parts[1], MessageContent::Image { .. }));
                assert!(matches!(&parts[2], MessageContent::File { .. }));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn url_less_attachment_skipped_keeps_valid_one() {
        let d = json!({
            "content": "",
            "attachments": [
                { "filename": "broken" },
                { "url": "https://cdn/ok.png", "filename": "ok.png", "content_type": "image/png" }
            ]
        });
        assert!(matches!(
            discord_message_content(&d),
            Some(MessageContent::Image { .. })
        ));
    }

    #[test]
    fn whitespace_only_text_is_none() {
        let d = json!({ "content": "   ", "attachments": [] });
        assert!(discord_message_content(&d).is_none());
    }
}

/// Build Discord button components from actionable controls.
///
/// `custom_id` is the literal command, returned verbatim in the
/// `INTERACTION_CREATE` payload — so a press routes to the same handler as the
/// typed command. Discord caps `custom_id` at 100 characters, looser than the
/// 64-byte cap `PromptAction::fits_callback_data` already enforced upstream.
fn message_components(actions: &[agentos_types::PromptAction]) -> serde_json::Value {
    use agentos_types::ActionStyle;
    if actions.is_empty() {
        return serde_json::Value::Null;
    }
    // Max 5 buttons per action row, max 5 rows.
    let rows: Vec<serde_json::Value> = actions
        .chunks(5)
        .take(5)
        .map(|row| {
            let buttons: Vec<serde_json::Value> = row
                .iter()
                .map(|a| {
                    let style = match a.style {
                        ActionStyle::Primary => 1,
                        ActionStyle::Secondary => 2,
                        ActionStyle::Danger => 4,
                    };
                    serde_json::json!({
                        "type": 2,
                        "style": style,
                        "label": a.short_label(80),
                        "custom_id": a.command,
                    })
                })
                .collect();
            serde_json::json!({ "type": 1, "components": buttons })
        })
        .collect();
    serde_json::json!(rows)
}

#[cfg(test)]
mod action_component_tests {
    use super::*;
    use agentos_types::{ActionStyle, PromptAction};

    fn actions() -> Vec<PromptAction> {
        vec![
            PromptAction::new("✅ Approve", "/approve 42", ActionStyle::Primary),
            PromptAction::new("❌ Deny", "/deny 42", ActionStyle::Danger),
            PromptAction::new(
                "✅ Approve & always allow",
                "/approve 42 always",
                ActionStyle::Secondary,
            ),
        ]
    }

    #[test]
    fn no_actions_yields_null_so_the_payload_key_is_omitted() {
        assert!(message_components(&[]).is_null());
    }

    #[test]
    fn custom_id_is_the_command_verbatim() {
        // A truncated custom_id would resolve a different escalation.
        let v = message_components(&actions());
        let row = &v[0]["components"];
        assert_eq!(row[0]["custom_id"], "/approve 42");
        assert_eq!(row[1]["custom_id"], "/deny 42");
        assert_eq!(row[2]["custom_id"], "/approve 42 always");
    }

    #[test]
    fn styles_map_to_discord_numbers() {
        let v = message_components(&actions());
        let row = &v[0]["components"];
        assert_eq!(row[0]["style"], 1, "primary");
        assert_eq!(row[1]["style"], 4, "danger");
        assert_eq!(row[2]["style"], 2, "secondary");
        assert_eq!(v[0]["type"], 1, "action row");
        assert_eq!(row[0]["type"], 2, "button");
    }

    #[test]
    fn rows_hold_at_most_five_buttons() {
        let many: Vec<PromptAction> = (0..7)
            .map(|i| PromptAction::new(format!("o{i}"), format!("/x {i}"), ActionStyle::Secondary))
            .collect();
        let v = message_components(&many);
        assert_eq!(v.as_array().expect("rows").len(), 2);
        assert_eq!(v[0]["components"].as_array().expect("row").len(), 5);
        assert_eq!(v[1]["components"].as_array().expect("row").len(), 2);
    }
}

/// Build an `InboundMessage` from a Discord `INTERACTION_CREATE` `d` object,
/// or `None` if it is not a component press this channel should act on.
///
/// Extracted from the gateway loop because this is the one place in the button
/// path where a wrong `platform_id` would hand escalation authority to the
/// wrong identity — it needs to be directly testable.
fn interaction_inbound(
    d: &serde_json::Value,
    channel_id: &str,
    instance_id: &str,
    raw: &serde_json::Value,
) -> Option<InboundMessage> {
    // 3 == MESSAGE_COMPONENT. Anything else (slash command, modal) is not ours.
    if d["type"].as_u64() != Some(3) {
        return None;
    }
    if d["channel_id"].as_str() != Some(channel_id) {
        return None;
    }
    let custom_id = d["data"]["custom_id"].as_str().unwrap_or("");
    if custom_id.is_empty() {
        return None;
    }
    // The presser, not the channel and not the message author: a guild button
    // press is under `member.user`, a DM press under `user`. Using the message
    // author would attribute the tap to the bot that posted the keyboard.
    let presser = if d["member"]["user"]["id"].is_string() {
        &d["member"]["user"]
    } else {
        &d["user"]
    };
    // Mirror the MESSAGE_CREATE loop guard. Components cannot be pressed by a
    // bot today, but the two paths should not disagree about it.
    if presser["bot"].as_bool() == Some(true) {
        return None;
    }
    Some(InboundMessage {
        id: d["id"].as_str().unwrap_or("").to_string(),
        channel_type: "discord".to_string(),
        channel_instance_id: instance_id.to_string(),
        sender: ChannelIdentity {
            platform_id: presser["id"].as_str().unwrap_or("").to_string(),
            display_name: presser["username"].as_str().map(String::from),
        },
        content: MessageContent::Text(custom_id.to_string()),
        thread_id: None,
        timestamp: chrono::Utc::now(),
        raw: raw.clone(),
    })
}

#[cfg(test)]
mod interaction_tests {
    use super::*;

    fn press(overrides: serde_json::Value) -> serde_json::Value {
        let mut d = serde_json::json!({
            "id": "i1",
            "type": 3,
            "channel_id": "chan-1",
            "token": "tok",
            "data": { "custom_id": "/approve 42" },
            "member": { "user": { "id": "user-9", "username": "ajas" } }
        });
        if let (Some(base), Some(ov)) = (d.as_object_mut(), overrides.as_object()) {
            for (k, v) in ov {
                base.insert(k.clone(), v.clone());
            }
        }
        d
    }

    #[test]
    fn guild_press_is_attributed_to_the_presser_not_the_bot() {
        let d = press(serde_json::json!({}));
        let m = interaction_inbound(&d, "chan-1", "inst-1", &d).expect("press must yield inbound");
        // This is the pairing-allowlist identity — the same field the
        // MESSAGE_CREATE arm fills from `author.id`.
        assert_eq!(m.sender.platform_id, "user-9");
        assert_eq!(m.content.as_text(), "/approve 42");
    }

    #[test]
    fn dm_press_reads_the_top_level_user() {
        let d = press(serde_json::json!({
            "member": serde_json::Value::Null,
            "user": { "id": "dm-user", "username": "ajas" }
        }));
        let m = interaction_inbound(&d, "chan-1", "inst-1", &d).expect("dm press");
        assert_eq!(m.sender.platform_id, "dm-user");
    }

    #[test]
    fn press_from_another_channel_is_ignored() {
        // Without this the bot would accept approvals from any channel it can
        // see, bypassing the pairing scope.
        let d = press(serde_json::json!({ "channel_id": "other" }));
        assert!(interaction_inbound(&d, "chan-1", "inst-1", &d).is_none());
    }

    #[test]
    fn non_component_interactions_are_ignored() {
        let d = press(serde_json::json!({ "type": 2 }));
        assert!(interaction_inbound(&d, "chan-1", "inst-1", &d).is_none());
    }

    #[test]
    fn empty_custom_id_is_ignored() {
        let d = press(serde_json::json!({ "data": { "custom_id": "" } }));
        assert!(interaction_inbound(&d, "chan-1", "inst-1", &d).is_none());
    }

    #[test]
    fn bot_press_is_ignored_like_bot_messages() {
        let d = press(serde_json::json!({
            "member": { "user": { "id": "b1", "username": "bot", "bot": true } }
        }));
        assert!(interaction_inbound(&d, "chan-1", "inst-1", &d).is_none());
    }
}
