use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use regex::Regex;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use std::fs::OpenOptions;
use std::io::Write;
use tokio::sync::broadcast;

use crate::app::{ChatInfo, ChatSection};

/// Updates received from Slack
#[derive(Debug, Clone)]
pub enum SlackUpdate {
    NewMessage {
        channel_id: String,
        user_name: String,
        text: String,
        ts: String,
        thread_ts: Option<String>,
        is_bot: bool,
        is_self: bool,
        forwarded: Option<String>,
        mentions_me: bool,
        files: Vec<SlackFile>,
    },
    MessageChanged {
        channel_id: String,
        ts: String,
        new_text: String,
    },
    MessageDeleted {
        channel_id: String,
        ts: String,
    },
    UserTyping {
        channel_id: String,
        user_name: String,
    },
}

#[derive(Clone)]
pub struct SlackClient {
    http: HttpClient,
    token: String, // Can be either User Token (xoxp-) or Bot Token (xoxb-)
    user_id: Arc<Mutex<Option<String>>>,
    pending_updates: Arc<Mutex<Vec<SlackUpdate>>>,
    ws_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ws_shutdown: Arc<Mutex<Option<broadcast::Sender<()>>>>,
    user_name_cache: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct AuthTestResponse {
    ok: bool,
    user_id: String,
    team: String,
    team_id: String,
}

#[derive(Deserialize)]
struct ConversationsListResponse {
    ok: bool,
    channels: Vec<Channel>,
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    is_group: bool,
    #[serde(default)]
    is_im: bool,
    #[serde(default)]
    is_mpim: bool,
    #[serde(default)]
    is_private: bool,
    #[serde(default)]
    is_archived: bool,
    #[serde(default)]
    is_member: bool,
    #[serde(default)]
    unread_count: Option<u32>,
}

#[derive(Deserialize)]
struct ConversationMembersResponse {
    ok: bool,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Deserialize)]
struct ConversationHistoryResponse {
    ok: bool,
    messages: Vec<SlackMessage>,
    #[serde(default)]
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Deserialize)]
struct ResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SlackReaction {
    pub name: String,
    pub count: u32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SlackMessage {
    #[serde(rename = "type", default)]
    pub msg_type: String,
    pub ts: String,
    pub user: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub bot_profile: Option<BotProfile>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub reply_count: Option<u32>,
    #[serde(default)]
    pub attachments: Vec<SlackAttachment>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct BotProfile {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SlackFile {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub filetype: Option<String>,
    #[serde(default)]
    pub url_private: Option<String>,
    #[serde(default)]
    pub url_private_download: Option<String>,
    #[serde(default)]
    pub thumb_64: Option<String>,
    #[serde(default)]
    pub thumb_360: Option<String>,
    #[serde(default)]
    pub thumb_480: Option<String>,
    #[serde(default)]
    pub thumb_720: Option<String>,
    #[serde(default)]
    pub thumb_800: Option<String>,
    #[serde(default)]
    pub thumb_960: Option<String>,
    #[serde(default)]
    pub thumb_1024: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SlackAttachment {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub pretext: Option<String>,
    #[serde(default)]
    pub author_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

fn extract_forwarded_text(attachments: &[SlackAttachment]) -> Option<String> {
    for att in attachments {
        if let Some(text) = att.text.as_ref().filter(|t| !t.is_empty()) {
            return Some(text.clone());
        }
        if let Some(pretext) = att.pretext.as_ref().filter(|t| !t.is_empty()) {
            return Some(pretext.clone());
        }
        if let Some(fallback) = att.fallback.as_ref().filter(|t| !t.is_empty()) {
            return Some(fallback.clone());
        }
    }
    None
}

/// Check if the text contains a mention of the specified user ID
/// Looks for patterns like <@U12345> or <@U12345|name>
fn text_mentions_user(text: &str, user_id: &str) -> bool {
    if user_id.is_empty() {
        return false;
    }
    
    // Look for <@USER_ID> or <@USER_ID|...>
    let pattern1 = format!("<@{}>", user_id);
    let pattern2 = format!("<@{}|", user_id);
    
    text.contains(&pattern1) || text.contains(&pattern2)
}

#[derive(Deserialize)]
struct UserInfoResponse {
    ok: bool,
    user: User,
}

#[derive(Deserialize)]
struct UserProfile {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct User {
    id: String,
    name: String,
    real_name: Option<String>,
    #[serde(default)]
    profile: Option<UserProfile>,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize)]
struct SocketModeConnectResponse {
    ok: bool,
    url: String,
}

impl SlackClient {
    pub async fn new(token: &str, _app_token: &str) -> Result<Self> {
        let http = HttpClient::new();
        let token = token.to_string();

        let client = Self {
            http,
            token,
            user_id: Arc::new(Mutex::new(None)),
            pending_updates: Arc::new(Mutex::new(Vec::new())),
            ws_handle: Arc::new(Mutex::new(None)),
            ws_shutdown: Arc::new(Mutex::new(None)),
            user_name_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };

        // Test authentication
        let auth_response: AuthTestResponse = client
            .http
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&client.token)
            .send()
            .await?
            .json()
            .await?;

        if !auth_response.ok {
            return Err(anyhow!("Slack authentication failed"));
        }

        *client.user_id.lock().await = Some(auth_response.user_id);

        Ok(client)
    }

    pub async fn get_my_user_id(&self) -> Result<String> {
        let user_id = self.user_id.lock().await;
        user_id.clone().ok_or_else(|| anyhow!("User ID not set"))
    }

    pub async fn start_event_listener(&self, app_token: String) -> Result<()> {
        // Log that we're starting a new listener
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/slack_rust_debug.log")
            .and_then(|mut f| {
                use std::io::Write;
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                writeln!(f, "[{}] start_event_listener called", timestamp)
            });

        let pending_updates = self.pending_updates.clone();
        let http = self.http.clone();
        let token = self.token.clone();
        let user_id = self.user_id.clone();

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);
        *self.ws_shutdown.lock().await = Some(shutdown_tx);

        // Channel for proactive reconnection (when approximate_connection_time elapses)
        let (proactive_tx, mut proactive_rx) = mpsc::channel::<()>(1);

        let handle = tokio::spawn(async move {
            let log_to_file = |msg: &str| {
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/slack_rust_debug.log")
                {
                    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                    let _ = writeln!(file, "[{}] {}", timestamp, msg);
                }
            };

            let envelope_id_regex = Regex::new(r#""envelope_id"\s*:\s*"([^"]+)""#).expect("valid regex");

            log_to_file("WebSocket task starting...");

            // Reconnection loop
            'reconnect: loop {
                // Get fresh WebSocket URL (Slack rotates these periodically)
                let ws_url = match http
                    .post("https://slack.com/api/apps.connections.open")
                    .bearer_auth(&app_token)
                    .send()
                    .await
                {
                    Ok(resp) => match resp.json::<SocketModeConnectResponse>().await {
                        Ok(r) if r.ok => r.url,
                        Ok(_) => {
                            log_to_file("apps.connections.open returned ok=false");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue 'reconnect;
                        }
                        Err(e) => {
                            log_to_file(&format!("apps.connections.open parse error: {}", e));
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue 'reconnect;
                        }
                    },
                    Err(e) => {
                        log_to_file(&format!("apps.connections.open request failed: {}", e));
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue 'reconnect;
                    }
                };

                let (mut ws_stream, _) = match connect_async(&ws_url).await {
                    Ok(conn) => conn,
                    Err(e) => {
                        log_to_file(&format!("WebSocket connect failed: {}", e));
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue 'reconnect;
                    }
                };

                log_to_file("WebSocket connected successfully");

                // Process messages until disconnect, stream end, or shutdown
                loop {
                    tokio::select! {
                        biased;

                        _ = shutdown_rx.recv() => {
                            log_to_file("Received shutdown signal, closing WebSocket gracefully");
                            let _ = ws_stream.close(None).await;
                            break 'reconnect;
                        }

                        Some(()) = proactive_rx.recv() => {
                            log_to_file("Proactive reconnect triggered (before connection timeout)");
                            let _ = ws_stream.close(None).await;
                            break;
                        }

                        msg = ws_stream.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    log_to_file(&format!("Received WebSocket message: {}", &text[..text.len().min(200)]));

                                    // Robust ack: extract envelope_id even if full parse fails
                                    let envelope_id = serde_json::from_str::<serde_json::Value>(&text)
                                        .ok()
                                        .and_then(|e| e.get("envelope_id").and_then(|v| v.as_str()).map(String::from))
                                        .or_else(|| {
                                            envelope_id_regex
                                                .captures(&text)
                                                .and_then(|c| c.get(1))
                                                .map(|m| m.as_str().to_string())
                                        });

                                    if let Some(ref eid) = envelope_id {
                                        let ack = serde_json::json!({ "envelope_id": eid });
                                        let _ = ws_stream.send(Message::Text(ack.to_string())).await;
                                        log_to_file(&format!("Acknowledged envelope: {}", eid));
                                    }

                                    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&text) {
                                        if let Some(event_type) = envelope.get("type").and_then(|v| v.as_str()) {
                                            log_to_file(&format!("Event type: {}", event_type));

                                            if event_type == "hello" {
                                                if let Some(debug) = envelope.get("debug_info") {
                                                    if let Some(secs) = debug.get("approximate_connection_time").and_then(|v| v.as_u64()) {
                                                        let delay_secs = secs.saturating_sub(100).max(60);
                                                        let proactive_tx_clone = proactive_tx.clone();
                                                        tokio::spawn(async move {
                                                            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                                                            let _ = proactive_tx_clone.send(()).await;
                                                        });
                                                        log_to_file(&format!("Scheduled proactive reconnect in {} seconds", delay_secs));
                                                    }
                                                }
                                            } else if event_type == "disconnect" {
                                                let reason = envelope.get("reason").and_then(|v| v.as_str()).unwrap_or("unknown");
                                                log_to_file(&format!("Received disconnect (reason: {}), reconnecting", reason));
                                                let _ = ws_stream.close(None).await;
                                                break;
                                            } else if event_type == "events_api" {
                                                if let Some(event) = envelope.get("payload").and_then(|p| p.get("event")) {
                                                    log_to_file(&format!("Processing event: {:?}", event));
                                                    Self::process_event(
                                                        event,
                                                        &pending_updates,
                                                        &http,
                                                        &token,
                                                        &user_id,
                                                    )
                                                    .await;
                                                    log_to_file("Event processed, added to pending_updates");
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Ok(Message::Close(_))) => {
                                    log_to_file("WebSocket received Close frame, reconnecting");
                                    break;
                                }
                                Some(Err(e)) => {
                                    log_to_file(&format!("WebSocket stream error: {}", e));
                                    break;
                                }
                                None => {
                                    log_to_file("WebSocket stream ended, reconnecting");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            log_to_file("WebSocket task exiting");
        });

        *self.ws_handle.lock().await = Some(handle);
        Ok(())
    }

    async fn process_event(
        event: &serde_json::Value,
        pending_updates: &Arc<Mutex<Vec<SlackUpdate>>>,
        http: &HttpClient,
        token: &str,
        user_id: &Arc<Mutex<Option<String>>>,
    ) {
        // Local logging function
        let log_to_file = |msg: &str| {
            use std::fs::OpenOptions;
            use std::io::Write;
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };

        if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
            match event_type {
                "message" => {
                    // Check for message subtypes (edited, deleted)
                    let subtype = event.get("subtype").and_then(|v| v.as_str());
                    
                    match subtype {
                        Some("message_changed") => {
                            // Message was edited
                            if let (Some(channel_id), Some(message)) = (
                                event.get("channel").and_then(|v| v.as_str()),
                                event.get("message"),
                            ) {
                                if let (Some(ts), Some(new_text)) = (
                                    message.get("ts").and_then(|v| v.as_str()),
                                    message.get("text").and_then(|v| v.as_str()),
                                ) {
                                    pending_updates.lock().await.push(SlackUpdate::MessageChanged {
                                        channel_id: channel_id.to_string(),
                                        ts: ts.to_string(),
                                        new_text: new_text.to_string(),
                                    });
                                }
                            }
                            return;
                        }
                        Some("message_deleted") => {
                            // Message was deleted
                            if let (Some(channel_id), Some(deleted_ts)) = (
                                event.get("channel").and_then(|v| v.as_str()),
                                event.get("deleted_ts").and_then(|v| v.as_str()),
                            ) {
                                pending_updates.lock().await.push(SlackUpdate::MessageDeleted {
                                    channel_id: channel_id.to_string(),
                                    ts: deleted_ts.to_string(),
                                });
                            }
                            return;
                        }
                        _ => {}
                    }
                    
                    // Regular new message
                    if let (Some(channel_id), Some(text), Some(ts)) = (
                        event.get("channel").and_then(|v| v.as_str()),
                        event.get("text").and_then(|v| v.as_str()),
                        event.get("ts").and_then(|v| v.as_str()),
                    ) {
                        let user_id_event = event
                            .get("user")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let is_bot = event.get("bot_id").is_some();
                        let thread_ts = event
                            .get("thread_ts")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let attachments: Vec<SlackAttachment> = event
                            .get("attachments")
                            .and_then(|a| serde_json::from_value(a.clone()).ok())
                            .unwrap_or_default();
                        let forwarded = extract_forwarded_text(&attachments);

                        let my_id = user_id.lock().await.clone().unwrap_or_default();
                        let is_self = !my_id.is_empty() && user_id_event == my_id;
                        
                        // Check if the message mentions the current user
                        let mentions_me = !my_id.is_empty() && text_mentions_user(text, &my_id);

                        // DEBUG: Log the entire event to see what fields we have
                        log_to_file("=== MESSAGE EVENT DEBUG ===");
                        log_to_file(&format!("Full event: {}", serde_json::to_string_pretty(event).unwrap_or_default()));
                        log_to_file(&format!("user field: {:?}", event.get("user")));
                        log_to_file(&format!("username field: {:?}", event.get("username")));
                        log_to_file(&format!("bot_id field: {:?}", event.get("bot_id")));
                        log_to_file(&format!("bot_profile field: {:?}", event.get("bot_profile")));
                        log_to_file(&format!("app_id field: {:?}", event.get("app_id")));

                        // Fetch user name - prioritize user field first (real users), then bot_profile, username, bot_id
                        let user_name = if event.get("user").is_some() && user_id_event != "unknown" {
                            // Regular user - fetch from API (prioritize this over bot_profile)
                            if let Ok(user_info) = Self::fetch_user_info(http, token, user_id_event).await {
                                log_to_file(&format!("Using fetched user info: {}", user_info));
                                user_info
                            } else {
                                log_to_file(&format!("Failed to fetch user info, using user_id: {}", user_id_event));
                                user_id_event.to_string()
                            }
                        } else if let Some(bot_profile) = event.get("bot_profile") {
                            // Slack app/webhook with bot_profile (only if no user field)
                            let name = bot_profile
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("Bot")
                                .to_string();
                            log_to_file(&format!("Using bot_profile.name: {}", name));
                            name
                        } else if let Some(username) = event.get("username").and_then(|u| u.as_str()) {
                            // Bot with username field
                            log_to_file(&format!("Using username field: {}", username));
                            username.to_string()
                        } else if let Some(bot_id) = event.get("bot_id").and_then(|b| b.as_str()) {
                            // Bot message - fetch bot info
                            log_to_file(&format!("Fetching bot info for bot_id: {}", bot_id));
                            let client = SlackClient {
                                http: http.clone(),
                                token: token.to_string(),
                                user_id: user_id.clone(),
                                pending_updates: pending_updates.clone(),
                                ws_handle: Arc::new(Mutex::new(None)),
                                ws_shutdown: Arc::new(Mutex::new(None)),
                                user_name_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
                            };
                            let bot_name = client.resolve_bot_name(bot_id).await;
                            log_to_file(&format!("Got bot name: {}", bot_name));
                            bot_name
                        } else {
                            log_to_file(&format!("No user info available, using user_id_event: {}", user_id_event));
                            user_id_event.to_string()
                        };
                        log_to_file(&format!("Final user_name: {}", user_name));

                        // Extract files from event
                        log_to_file(&format!("Files field in event: {:?}", event.get("files")));
                        let files: Vec<SlackFile> = event
                            .get("files")
                            .and_then(|f| {
                                log_to_file(&format!("Files JSON: {}", serde_json::to_string_pretty(f).unwrap_or_default()));
                                serde_json::from_value(f.clone()).ok()
                            })
                            .unwrap_or_default();
                        log_to_file(&format!("Parsed {} files", files.len()));
                        for (idx, file) in files.iter().enumerate() {
                            log_to_file(&format!("  File {}: id={:?}, mimetype={:?}, filetype={:?}, name={:?}", 
                                idx, file.id, file.mimetype, file.filetype, file.name));
                        }

                        pending_updates.lock().await.push(SlackUpdate::NewMessage {
                            channel_id: channel_id.to_string(),
                            user_name,
                            text: text.to_string(),
                            ts: ts.to_string(),
                            thread_ts,
                            is_bot,
                            is_self,
                            forwarded,
                            mentions_me,
                            files,
                        });
                    }
                }
                "user_typing" => {
                    if let (Some(channel_id), Some(user_id)) = (
                        event.get("channel").and_then(|v| v.as_str()),
                        event.get("user").and_then(|v| v.as_str()),
                    ) {
                        let user_name = if let Ok(user_info) =
                            Self::fetch_user_info(http, token, user_id).await
                        {
                            user_info
                        } else {
                            user_id.to_string()
                        };

                        pending_updates.lock().await.push(SlackUpdate::UserTyping {
                            channel_id: channel_id.to_string(),
                            user_name,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    pub async fn resolve_user_name(&self, user_id: &str) -> String {
        // Check cache first
        {
            let cache = self.user_name_cache.lock().await;
            if let Some(name) = cache.get(user_id) {
                return name.clone();
            }
        }
        // Fetch and cache
        let name = Self::fetch_user_info(&self.http, &self.token, user_id)
            .await
            .unwrap_or_else(|_| user_id.to_string());
        self.user_name_cache
            .lock()
            .await
            .insert(user_id.to_string(), name.clone());
        name
    }

    /// Get a snapshot of the user name cache for synchronous lookups.
    pub async fn get_user_name_cache(&self) -> std::collections::HashMap<String, String> {
        self.user_name_cache.lock().await.clone()
    }

    async fn fetch_user_info(http: &HttpClient, token: &str, user_id: &str) -> Result<String> {
        let response: UserInfoResponse = http
            .get(&format!(
                "https://slack.com/api/users.info?user={}",
                user_id
            ))
            .bearer_auth(token)
            .send()
            .await?
            .json()
            .await?;

        if response.ok {
            // Prefer display_name > name (username)
            let display_name = response
                .user
                .profile
                .and_then(|p| p.display_name)
                .filter(|n| !n.is_empty());
            Ok(display_name.unwrap_or(response.user.name))
        } else {
            Ok(user_id.to_string())
        }
    }

    pub async fn is_user_bot(&self, user_id: &str) -> bool {
        let resp = self
            .http
            .get(&format!(
                "https://slack.com/api/users.info?user={}",
                user_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(info) = resp.json::<UserInfoResponse>().await {
                if info.ok {
                    return info.user.is_bot;
                }
            }
        }
        false
    }

    pub async fn is_user_deleted(&self, user_id: &str) -> bool {
        let resp = self
            .http
            .get(&format!(
                "https://slack.com/api/users.info?user={}",
                user_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(info) = resp.json::<UserInfoResponse>().await {
                if info.ok {
                    return info.user.deleted;
                }
            }
        }
        false
    }

    pub async fn resolve_bot_name(&self, bot_id: &str) -> String {
        // Check cache first
        {
            let cache = self.user_name_cache.lock().await;
            if let Some(name) = cache.get(bot_id) {
                return name.clone();
            }
        }
        
        // Fetch bot info
        let resp = self
            .http
            .get(&format!(
                "https://slack.com/api/bots.info?bot={}",
                bot_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    if let Some(name) = json.get("bot")
                        .and_then(|b| b.get("name"))
                        .and_then(|n| n.as_str()) {
                        let name_str = name.to_string();
                        // Cache it
                        self.user_name_cache
                            .lock()
                            .await
                            .insert(bot_id.to_string(), name_str.clone());
                        return name_str;
                    }
                }
            }
        }
        
        bot_id.to_string()
    }

    pub async fn get_conversation_members(&self, channel_id: &str) -> Result<Vec<String>> {
        let response: ConversationMembersResponse = self
            .http
            .get(&format!(
                "https://slack.com/api/conversations.members?channel={}&limit=100",
                channel_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        if !response.ok {
            return Err(anyhow!("Failed to fetch conversation members"));
        }

        Ok(response.members)
    }

    pub async fn get_conversations(&self) -> Result<Vec<ChatInfo>> {
        let response: ConversationsListResponse = self
            .http
            .get("https://slack.com/api/conversations.list?types=public_channel,private_channel,mpim,im&limit=200")
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        if !response.ok {
            return Err(anyhow!("Failed to fetch conversations"));
        }

        let my_user_id = self.get_my_user_id().await.unwrap_or_default();

        let mut chats = Vec::new();
        for ch in response.channels {
            if ch.is_archived {
                continue;
            }
            
            // Skip channels we're not a member of (except for DMs which don't have is_member)
            if !ch.is_im && !ch.is_mpim && !ch.is_member {
                continue;
            }
            
            // Skip DMs with deleted users
            if ch.is_im {
                if let Some(ref uid) = ch.user {
                    if self.is_user_deleted(uid).await {
                        continue;
                    }
                }
            }

            // Determine section
            let section = if ch.is_mpim {
                ChatSection::Group
            } else if ch.is_im {
                // Check if DM target is a bot
                let is_bot = if let Some(ref uid) = ch.user {
                    self.is_user_bot(uid).await
                } else {
                    false
                };
                if is_bot {
                    ChatSection::Bot
                } else {
                    ChatSection::DirectMessage
                }
            } else if ch.is_private || ch.is_group {
                ChatSection::Private
            } else {
                ChatSection::Public
            };

            let name = match section {
                ChatSection::Group => {
                    // Fetch members and build "Name1, Name2" excluding self
                    match self.get_conversation_members(&ch.id).await {
                        Ok(members) => {
                            let mut names = Vec::new();
                            for mid in &members {
                                if mid != &my_user_id {
                                    let n = self.resolve_user_name(mid).await;
                                    // Use first name only
                                    let first =
                                        n.split_whitespace().next().unwrap_or(&n).to_string();
                                    names.push(first);
                                }
                            }
                            if names.is_empty() {
                                ch.name.unwrap_or_else(|| ch.id.clone())
                            } else {
                                names.join(", ")
                            }
                        }
                        Err(_) => ch.name.unwrap_or_else(|| ch.id.clone()),
                    }
                }
                ChatSection::DirectMessage | ChatSection::Bot => {
                    if let Some(ref user_id) = ch.user {
                        self.resolve_user_name(user_id).await
                    } else {
                        ch.name.unwrap_or_else(|| ch.id.clone())
                    }
                }
                _ => ch.name.unwrap_or_else(|| ch.id.clone()),
            };

            chats.push(ChatInfo {
                id: ch.id.clone(),
                name,
                username: ch.user.or(Some(ch.id)),
                unread: ch.unread_count.unwrap_or(0),
                section,
            });
        }

        Ok(chats)
    }

    pub async fn get_conversation_history(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<SlackMessage>> {
        let mut all_messages: Vec<SlackMessage> = Vec::new();
        let mut cursor: Option<String> = None;
        let page_limit = limit.min(200).max(1);

        loop {
            let mut url = format!(
                "https://slack.com/api/conversations.history?channel={}&limit={}",
                channel_id, page_limit
            );
            if let Some(ref c) = cursor {
                url.push_str(&format!("&cursor={}", c));
            }

            let response: ConversationHistoryResponse = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .send()
                .await?
                .json()
                .await?;

            if !response.ok {
                return Err(anyhow!("Failed to fetch conversation history"));
            }

            all_messages.extend(response.messages);
            if all_messages.len() >= limit {
                all_messages.truncate(limit);
                break;
            }

            let next_cursor = response
                .response_metadata
                .and_then(|m| {
                    if m.next_cursor.trim().is_empty() {
                        None
                    } else {
                        Some(m.next_cursor)
                    }
                });

            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(all_messages)
    }

    pub async fn get_thread_replies(
        &self,
        channel_id: &str,
        thread_ts: &str,
        limit: usize,
    ) -> Result<Vec<SlackMessage>> {
        let mut all_messages: Vec<SlackMessage> = Vec::new();
        let mut cursor: Option<String> = None;
        let page_limit = limit.min(200).max(1);

        loop {
            let mut url = format!(
                "https://slack.com/api/conversations.replies?channel={}&ts={}&limit={}",
                channel_id, thread_ts, page_limit
            );
            if let Some(ref c) = cursor {
                url.push_str(&format!("&cursor={}", c));
            }

            let response: ConversationHistoryResponse = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .send()
                .await?
                .json()
                .await?;

            if !response.ok {
                return Err(anyhow!("Failed to fetch thread replies"));
            }

            all_messages.extend(response.messages);
            if all_messages.len() >= limit {
                all_messages.truncate(limit);
                break;
            }

            let next_cursor = response
                .response_metadata
                .and_then(|m| {
                    if m.next_cursor.trim().is_empty() {
                        None
                    } else {
                        Some(m.next_cursor)
                    }
                });

            match next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(all_messages)
    }

    pub async fn send_message(
        &self,
        channel_id: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        let mut payload = serde_json::json!({
            "channel": channel_id,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            payload["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let response: serde_json::Value = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        if !response
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(anyhow!("Failed to send message"));
        }

        Ok(())
    }

    pub async fn add_reaction(&self, channel_id: &str, timestamp: &str, emoji: &str) -> Result<()> {
        let payload = serde_json::json!({
            "channel": channel_id,
            "timestamp": timestamp,
            "name": emoji,
        });

        let response: serde_json::Value = self
            .http
            .post("https://slack.com/api/reactions.add")
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        if !response
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(anyhow!("Failed to add reaction"));
        }

        Ok(())
    }

    pub async fn leave_conversation(&self, channel_id: &str) -> Result<()> {
        let payload = serde_json::json!({
            "channel": channel_id,
        });

        let response: serde_json::Value = self
            .http
            .post("https://slack.com/api/conversations.leave")
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await?
            .json()
            .await?;

        if !response
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(anyhow!("Failed to leave conversation"));
        }

        Ok(())
    }

    pub async fn get_pending_updates(&self) -> Vec<SlackUpdate> {
        let mut updates = self.pending_updates.lock().await;
        std::mem::take(&mut *updates)
    }

    #[allow(dead_code)]
    pub async fn download_file(&self, file_id: &str, _channel_id: &str) -> Result<std::path::PathBuf> {
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let log_to_file = |msg: &str| {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };
        
        log_to_file(&format!("=== DOWNLOAD FILE DEBUG ==="));
        log_to_file(&format!("file_id: {}", file_id));
        
        // First, get file info to get the download URL
        let file_info_url = format!("https://slack.com/api/files.info?file={}", file_id);
        log_to_file(&format!("Requesting file info from: {}", file_info_url));
        
        let file_info_response: serde_json::Value = self
            .http
            .get(&file_info_url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        log_to_file(&format!("File info response: {}", serde_json::to_string_pretty(&file_info_response).unwrap_or_default()));

        if !file_info_response.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let error = file_info_response.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            log_to_file(&format!("Failed to get file info: {}", error));
            return Err(anyhow!("Failed to get file info: {}", error));
        }

        let file = file_info_response.get("file").ok_or_else(|| {
            log_to_file("No file data in response");
            anyhow!("No file data")
        })?;
        
        log_to_file(&format!("File data: {}", serde_json::to_string_pretty(file).unwrap_or_default()));
        
        let url_private = file.get("url_private")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                log_to_file("No url_private in file data");
                anyhow!("No download URL")
            })?;
        
        log_to_file(&format!("Download URL: {}", url_private));
        
        let file_name = file.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        
        log_to_file(&format!("File name: {}", file_name));
        
        // Create store directory if it doesn't exist
        let store_dir = std::path::Path::new("store");
        log_to_file(&format!("Creating store directory: {:?}", store_dir));
        std::fs::create_dir_all(store_dir)?;
        
        // Download the file
        log_to_file("Starting file download...");
        let response = self
            .http
            .get(url_private)
            .bearer_auth(&self.token)
            .send()
            .await?;
        
        log_to_file(&format!("Download response status: {}", response.status()));
        
        if !response.status().is_success() {
            log_to_file(&format!("Download failed with status: {}", response.status()));
            return Err(anyhow!("Failed to download file: {}", response.status()));
        }
        
        let file_path = store_dir.join(file_name);
        log_to_file(&format!("Saving file to: {:?}", file_path));
        
        let mut file = std::fs::File::create(&file_path)?;
        let bytes = response.bytes().await?;
        log_to_file(&format!("Received {} bytes", bytes.len()));
        
        file.write_all(&bytes)?;
        log_to_file(&format!("File saved successfully to: {:?}", file_path));
        
        Ok(file_path)
    }

    /// Extract redirect URL from HTML response (handles meta refresh, window.location, etc.)
    fn extract_redirect_from_html(html: &str) -> Option<String> {
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let log_to_file = |msg: &str| {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };
        
        log_to_file("=== EXTRACT REDIRECT FROM HTML ===");
        
        // First, try to find URL in JSON data (data-props, entryPoint, etc.)
        // Look for "entryPoint":"https:\/\/files.slack.com...
        if let Some(entry_start) = html.find("\"entryPoint\"") {
            log_to_file("Found entryPoint in JSON data");
            let after_entry = &html[entry_start..];
            // Look for the URL after entryPoint
            if let Some(url_start_pos) = after_entry.find("https:\\/\\/files.slack.com") {
                let url_part = &after_entry[url_start_pos..];
                // Find the end of the URL (until quote or comma)
                let mut url_end = url_part.len();
                for (i, c) in url_part.char_indices() {
                    if c == '"' || c == ',' || c == '}' {
                        url_end = i;
                        break;
                    }
                }
                // Also check for HTML entities like &quot; at the end
                if let Some(amp_pos) = url_part[..url_end].rfind('&') {
                    if url_part[amp_pos..].starts_with("&quot;") || url_part[amp_pos..].starts_with("&amp;") {
                        url_end = amp_pos;
                    }
                }
                let escaped_url = &url_part[..url_end];
                // Unescape the URL
                let mut url = escaped_url.replace("\\/", "/").replace("\\\"", "\"").replace("\\'", "'");
                // Remove any trailing HTML entities or quotes
                url = url.trim_end_matches("&quot;").trim_end_matches("&amp;").trim_end_matches('"').trim_end_matches('\'').to_string();
                log_to_file(&format!("Found URL in entryPoint: {}", url));
                if url.starts_with("https://files.slack.com") && !url.contains("/beacon/") && !url.contains("/tracking/") {
                    return Some(url);
                }
            }
        }
        
        // Also look for escaped https://files.slack.com directly
        if let Some(start) = html.find("https:\\/\\/files.slack.com") {
            log_to_file("Found escaped https://files.slack.com");
            let url_part = &html[start..];
            let mut url_end = url_part.len();
            for (i, c) in url_part.char_indices() {
                if c == '"' || c == '\'' || c == ' ' || c == '>' || c == '<' || 
                   c == ')' || c == ';' || c == ',' || c == '}' || c == ']' || c == '\n' || c == '\r' {
                    url_end = i;
                    break;
                }
            }
            // Also check for HTML entities like &quot; at the end
            if url_end < url_part.len() && url_part[url_end..].starts_with("&quot;") {
                // Already stopped before &quot;
            } else if let Some(amp_pos) = url_part[..url_end].rfind('&') {
                // Check if there's an HTML entity at the end
                if url_part[amp_pos..].starts_with("&quot;") || url_part[amp_pos..].starts_with("&amp;") {
                    url_end = amp_pos;
                }
            }
            let escaped_url = &url_part[..url_end];
            let mut url = escaped_url.replace("\\/", "/").replace("\\\"", "\"").replace("\\'", "'");
            // Remove any trailing HTML entities or quotes
            url = url.trim_end_matches("&quot;").trim_end_matches("&amp;").trim_end_matches('"').trim_end_matches('\'').to_string();
            log_to_file(&format!("Found escaped URL: {}", url));
            if url.starts_with("https://files.slack.com") && !url.contains("/beacon/") && !url.contains("/tracking/") {
                return Some(url);
            }
        }
        
        // Then, find ALL occurrences of files.slack.com and extract the full URLs
        let mut search_start = 0;
        while let Some(start) = html[search_start..].find("files.slack.com") {
            let absolute_start = search_start + start;
            
            // Find the start of the URL (go backwards to find https:// or http://)
            let mut url_start = absolute_start;
            let mut found_protocol = false;
            // Look backwards up to 200 characters to find the protocol
            let max_lookback = absolute_start.min(200);
            for i in (0..max_lookback).rev() {
                let check_start = absolute_start.saturating_sub(i);
                if check_start + 7 <= html.len() && &html[check_start..check_start + 7] == "http://" {
                    url_start = check_start;
                    found_protocol = true;
                    break;
                }
                if check_start + 8 <= html.len() && &html[check_start..check_start + 8] == "https://" {
                    url_start = check_start;
                    found_protocol = true;
                    break;
                }
            }
            
            if found_protocol {
                log_to_file(&format!("Found protocol at position {}", url_start));
                // Find the end of the URL (until quote, space, or other delimiter)
                let url_part = &html[url_start..];
                let mut url_end = url_part.len();
                for (i, c) in url_part.char_indices() {
                    if c == '"' || c == '\'' || c == ' ' || c == '>' || c == '<' || 
                       c == ')' || c == ';' || c == ',' || c == '}' || c == ']' || c == '\n' || c == '\r' {
                        url_end = i;
                        break;
                    }
                }
                let url = url_part[..url_end].to_string();
                log_to_file(&format!("Found potential URL: {}", url));
                
                // Filter out tracking URLs - accept any files.slack.com URL that's not tracking
                if !url.contains("/beacon/") && !url.contains("/tracking/") && 
                   !url.contains("/analytics/") && !url.contains("/api/") {
                    // Unescape the URL if needed
                    let unescaped_url = url.replace("\\/", "/").replace("\\\"", "\"").replace("\\'", "'");
                    log_to_file(&format!("Unescaped URL: {}", unescaped_url));
                    // Make sure it's a valid URL
                    if unescaped_url.starts_with("http://") || unescaped_url.starts_with("https://") {
                        log_to_file(&format!("Returning valid URL: {}", unescaped_url));
                        return Some(unescaped_url);
                    } else {
                        log_to_file(&format!("URL doesn't start with http:// or https://"));
                    }
                } else {
                    log_to_file(&format!("URL filtered out (contains tracking/beacon/analytics/api)"));
                }
            } else {
                log_to_file(&format!("Could not find protocol before files.slack.com at position {}", absolute_start));
            }
            
            // Move search forward
            search_start = absolute_start + 1;
            if search_start >= html.len() {
                break;
            }
        }
        
        // Fallback: Look for direct download link (href to files.slack.com)
        if let Some(start) = html.find("href=\"https://files.slack.com") {
            let url_part = &html[start + 6..];
            if let Some(url_end) = url_part.find('"') {
                let url = url_part[..url_end].to_string();
                // Filter out tracking URLs
                if !url.contains("/beacon/") && !url.contains("/tracking/") {
                    return Some(url);
                }
            }
        }
        
        // Look for files.slack.com in any URL pattern (simple version)
        if let Some(start) = html.find("https://files.slack.com") {
            // Find the full URL (until quote, space, or end of string)
            let url_part = &html[start..];
            let mut url_end = url_part.len();
            for (i, c) in url_part.char_indices() {
                if c == '"' || c == '\'' || c == ' ' || c == '>' || c == '<' || c == ')' || c == ';' {
                    url_end = i;
                    break;
                }
            }
            let url = url_part[..url_end].to_string();
            // Filter out tracking URLs
            if !url.contains("/beacon/") && !url.contains("/tracking/") {
                return Some(url);
            }
        }
        
        // Look for meta refresh redirect (but filter out tracking URLs)
        if let Some(start) = html.find("http-equiv=\"refresh\"") {
            if let Some(content_start) = html[start..].find("content=\"") {
                let content = &html[start + content_start + 9..];
                if let Some(url_start) = content.find("url=") {
                    let url_part = &content[url_start + 4..];
                    if let Some(url_end) = url_part.find('"') {
                        let url = url_part[..url_end].to_string();
                        // Filter out tracking URLs
                        if !url.contains("/beacon/") && !url.contains("/tracking/") && url.contains("files.slack.com") {
                            return Some(url);
                        }
                    }
                }
            }
        }
        
        // Look for window.location redirect (but filter out tracking URLs)
        if let Some(start) = html.find("window.location") {
            let after_location = &html[start..];
            if let Some(url_start) = after_location.find("= \"") {
                let url_part = &after_location[url_start + 3..];
                if let Some(url_end) = url_part.find('"') {
                    let url = url_part[..url_end].to_string();
                    // Filter out tracking URLs and prioritize files.slack.com
                    if !url.contains("/beacon/") && !url.contains("/tracking/") && url.contains("files.slack.com") {
                        return Some(url);
                    }
                }
            }
            if let Some(url_start) = after_location.find("= '") {
                let url_part = &after_location[url_start + 3..];
                if let Some(url_end) = url_part.find('\'') {
                    let url = url_part[..url_end].to_string();
                    // Filter out tracking URLs and prioritize files.slack.com
                    if !url.contains("/beacon/") && !url.contains("/tracking/") && url.contains("files.slack.com") {
                        return Some(url);
                    }
                }
            }
        }
        
        log_to_file("No valid files.slack.com URL found in HTML");
        None
    }

    pub async fn download_file_from_url(&self, url: &str, file_name: &str) -> Result<std::path::PathBuf> {
        use std::collections::HashSet;
        
        let mut redirect_count = 0;
        let mut current_url = url.to_string();
        let mut tried_urls = HashSet::new();
        
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let log_to_file = |msg: &str| {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };
        
        loop {
            if redirect_count > 5 {
                return Err(anyhow!("Too many redirects (max 5)"));
            }
            
            // Check if we've already tried this URL (avoid infinite loops)
            if tried_urls.contains(&current_url) {
                log_to_file(&format!("URL redirect loop detected: already tried {}", current_url));
                return Err(anyhow!("URL redirect loop detected. The file URL requires authentication that we cannot provide. Try adding 'files:write:user' scope to your Slack app for direct file downloads."));
            }
            tried_urls.insert(current_url.clone());
            
            log_to_file(&format!("=== DOWNLOAD FILE FROM URL DEBUG (redirect {}) ===", redirect_count));
            log_to_file(&format!("URL: {}", current_url));
            log_to_file(&format!("File name: {}", file_name));
            
            // Create store directory if it doesn't exist
            let store_dir = std::path::Path::new("store");
            if redirect_count == 0 {
                log_to_file(&format!("Creating store directory: {:?}", store_dir));
                std::fs::create_dir_all(store_dir)?;
            }
            
            // Download the file directly from URL
            log_to_file("Starting file download from URL...");
            let request = self
                .http
                .get(&current_url)
                .bearer_auth(&self.token)
                .header("Accept", "*/*")
                .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36");
            
            // If this is a redirect, try to preserve cookies from previous request
            // (reqwest Client should handle this automatically, but we can be explicit)
            let response = request.send().await?;
        
        log_to_file(&format!("Download response status: {}", response.status()));
        
        // Log response headers
        let headers = response.headers();
        log_to_file("Response headers:");
        for (name, value) in headers.iter() {
            if let Ok(value_str) = value.to_str() {
                log_to_file(&format!("  {}: {}", name, value_str));
            } else {
                log_to_file(&format!("  {}: <binary>", name));
            }
        }
        
        // Check content-type - if it's HTML, something went wrong
        let content_type = headers.get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        log_to_file(&format!("Content-Type: {}", content_type));
        
            if content_type.contains("text/html") {
                log_to_file("WARNING: Received HTML instead of file. Attempting to extract redirect URL from HTML...");
                
                // Read the HTML response
                let html_bytes = response.bytes().await?;
                let html = String::from_utf8_lossy(&html_bytes);
                log_to_file(&format!("HTML response (first 1000 chars): {}", &html.chars().take(1000).collect::<String>()));
                
                // Also log if we can find any files.slack.com URLs in the HTML
                let mut search_pos = 0;
                let mut occurrence_count = 0;
                while let Some(pos) = html[search_pos..].find("files.slack.com") {
                    let absolute_pos = search_pos + pos;
                    occurrence_count += 1;
                    let start = absolute_pos.saturating_sub(100);
                    let end = (absolute_pos + 200).min(html.len());
                    let context = &html[start..end];
                    log_to_file(&format!("Context around files.slack.com #{}: ...{}...", occurrence_count, context));
                    search_pos = absolute_pos + 1;
                    if search_pos >= html.len() {
                        break;
                    }
                }
                log_to_file(&format!("Found {} mentions of 'files.slack.com' in HTML", occurrence_count));
                
                // Also try to find the URL in a different way - look for the file ID pattern
                if let Some(file_id_pos) = html.find("F0ACD4WMTV2") {
                    let start = file_id_pos.saturating_sub(50);
                    let end = (file_id_pos + 150).min(html.len());
                    let context = &html[start..end];
                    log_to_file(&format!("Context around file ID: ...{}...", context));
                }
                
                // Try to find a redirect URL in the HTML (common patterns)
                // Look for meta refresh, window.location, or direct download links
                if let Some(redirect_url) = Self::extract_redirect_from_html(&html) {
                    log_to_file(&format!("Found redirect URL in HTML: {}", redirect_url));
                    // Update URL and continue loop
                    current_url = redirect_url;
                    redirect_count += 1;
                    continue;
                }
                
                log_to_file("ERROR: Could not extract redirect URL from HTML.");
                return Err(anyhow!("Received HTML response instead of file, and could not find redirect URL."));
            }
            
            if !response.status().is_success() {
                log_to_file(&format!("Download failed with status: {}", response.status()));
                return Err(anyhow!("Failed to download file: {}", response.status()));
            }
            
            // Sanitize file name to avoid issues with special characters
            let sanitized_name = file_name
                .chars()
                .map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c })
                .collect::<String>();
            
            let file_path = store_dir.join(&sanitized_name);
            log_to_file(&format!("Saving file to: {:?} (sanitized from: {})", file_path, file_name));
            
            // Read all bytes and write to file
            let bytes = response.bytes().await?;
            log_to_file(&format!("Received {} bytes", bytes.len()));
            
            // Check first few bytes to verify it's valid
            if bytes.len() >= 8 {
                let header = &bytes[0..8.min(bytes.len())];
                log_to_file(&format!("File header (first {} bytes): {:?}", header.len(), header));
                
                // Verify it's not HTML
                if header.starts_with(b"<!DOCTYPE") || header.starts_with(b"<html") {
                    log_to_file("ERROR: File appears to be HTML, not a binary file!");
                    return Err(anyhow!("Downloaded file appears to be HTML, not the actual file."));
                }
            }
            
            let mut file = std::fs::File::create(&file_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?; // Ensure all data is written to disk
            log_to_file(&format!("File saved successfully to: {:?}", file_path));
            
            return Ok(file_path);
        }
    }

    pub async fn get_shared_public_url(&self, file_id: &str, file_name: &str) -> Result<std::path::PathBuf> {
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let log_to_file = |msg: &str| {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };
        
        log_to_file(&format!("=== GET SHARED PUBLIC URL DEBUG ==="));
        log_to_file(&format!("file_id: {}, file_name: {}", file_id, file_name));
        
        // Use files.sharedPublicURL API to get a direct download URL
        let share_url = format!("https://slack.com/api/files.sharedPublicURL?file={}", file_id);
        log_to_file(&format!("Requesting shared public URL from: {}", share_url));
        
        let share_response: serde_json::Value = self
            .http
            .get(&share_url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        log_to_file(&format!("Share response: {}", serde_json::to_string_pretty(&share_response).unwrap_or_default()));

        if !share_response.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let error = share_response.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            let needed = share_response.get("needed").and_then(|v| v.as_str()).unwrap_or("");
            log_to_file(&format!("Failed to get shared public URL: {} (needed: {})", error, needed));
            if error == "missing_scope" {
                return Err(anyhow!("Missing scope '{}'. Please add this scope to your Slack app's OAuth scopes and reinstall the app.", needed));
            }
            return Err(anyhow!("Failed to get shared public URL: {}", error));
        }

        // Get the download URL from the share response
        let file = share_response.get("file").ok_or_else(|| {
            log_to_file("No file data in share response");
            anyhow!("No file data in share response")
        })?;
        
        // Try permalink_public first (public share URL), then url_private_download
        let download_url = file.get("permalink_public")
            .or_else(|| file.get("url_private_download"))
            .or_else(|| file.get("url_private"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                log_to_file("No download URL in share response");
                anyhow!("No download URL in share response")
            })?;
        
        log_to_file(&format!("Got download URL from share: {}", download_url));
        
        // Now download the file
        self.download_file_from_url(download_url, file_name).await
    }

    #[allow(dead_code)]
    pub async fn download_file_by_id(&self, file_id: &str, file_name: &str) -> Result<std::path::PathBuf> {
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let log_to_file = |msg: &str| {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
            {
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        };
        
        log_to_file(&format!("=== DOWNLOAD FILE BY ID DEBUG ==="));
        log_to_file(&format!("file_id: {}, file_name: {}", file_id, file_name));
        
        // Get file info to get url_private_download
        let file_info_url = format!("https://slack.com/api/files.info?file={}", file_id);
        log_to_file(&format!("Requesting file info from: {}", file_info_url));
        
        let file_info_response: serde_json::Value = self
            .http
            .get(&file_info_url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        log_to_file(&format!("File info response: {}", serde_json::to_string_pretty(&file_info_response).unwrap_or_default()));

        if !file_info_response.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let error = file_info_response.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            log_to_file(&format!("Failed to get file info: {}", error));
            return Err(anyhow!("Failed to get file info: {}", error));
        }

        let file = file_info_response.get("file").ok_or_else(|| {
            log_to_file("No file data in response");
            anyhow!("No file data")
        })?;
        
        // Prefer url_private_download, fallback to url_private
        let download_url = file.get("url_private_download")
            .or_else(|| file.get("url_private"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                log_to_file("No download URL in file data");
                anyhow!("No download URL")
            })?;
        
        log_to_file(&format!("Got download URL: {}", download_url));
        
        // Now download the file
        self.download_file_from_url(download_url, file_name).await
    }

    /// Gracefully shutdown the background WebSocket task.
    pub async fn shutdown(&self) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/slack_rust_debug.log")
            .and_then(|mut f| {
                use std::io::Write;
                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                writeln!(f, "[{}] shutdown() called", timestamp)
            });
        
        // Send shutdown signal to gracefully close WebSocket
        if let Some(tx) = self.ws_shutdown.lock().await.take() {
            let _ = tx.send(());
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                    writeln!(f, "[{}] Shutdown signal sent", timestamp)
                });
        }
        
        // Wait for the task to finish (with timeout)
        if let Some(handle) = self.ws_handle.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/slack_rust_debug.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
                    writeln!(f, "[{}] WebSocket task finished", timestamp)
                });
        }
    }
}
