use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
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
        
        // Get WebSocket URL
        let response: SocketModeConnectResponse = self
            .http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&app_token)
            .send()
            .await?
            .json()
            .await?;

        if !response.ok {
            return Err(anyhow!("Failed to connect to Socket Mode"));
        }

        let pending_updates = self.pending_updates.clone();
        let http = self.http.clone();
        let token = self.token.clone();
        let user_id = self.user_id.clone();
        
        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);
        *self.ws_shutdown.lock().await = Some(shutdown_tx);

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

            log_to_file("WebSocket task starting...");
            if let Ok((mut ws_stream, _)) = connect_async(&response.url).await {
                log_to_file("WebSocket connected successfully");
                loop {
                    tokio::select! {
                        Some(Ok(msg)) = ws_stream.next() => {
                            if let Message::Text(text) = msg {
                                log_to_file(&format!("Received WebSocket message: {}", &text[..text.len().min(200)]));
                                if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&text) {
                                    // Acknowledge envelope
                                    if let Some(envelope_id) =
                                        envelope.get("envelope_id").and_then(|v| v.as_str())
                                    {
                                        let ack = serde_json::json!({
                                            "envelope_id": envelope_id
                                        });
                                        let _ = ws_stream.send(Message::Text(ack.to_string())).await;
                                        log_to_file(&format!("Acknowledged envelope: {}", envelope_id));
                                    }

                                    // Process event
                                    if let Some(event_type) = envelope.get("type").and_then(|v| v.as_str())
                                    {
                                        log_to_file(&format!("Event type: {}", event_type));
                                        if event_type == "events_api" {
                                            if let Some(event) =
                                                envelope.get("payload").and_then(|p| p.get("event"))
                                            {
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
                        }
                        _ = shutdown_rx.recv() => {
                            log_to_file("Received shutdown signal, closing WebSocket gracefully");
                            let _ = ws_stream.close(None).await;
                            log_to_file("WebSocket closed");
                            break;
                        }
                    }
                }
                log_to_file("WebSocket stream ended");
            } else {
                log_to_file("Failed to connect WebSocket");
            }
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

                        // Fetch user name - prioritize bot_profile, then username, then bot_id lookup, then user lookup
                        let user_name = if let Some(bot_profile) = event.get("bot_profile") {
                            // Slack app/webhook with bot_profile
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
                        } else if event.get("user").is_some() {
                            // Regular user - fetch from API
                            if let Ok(user_info) = Self::fetch_user_info(http, token, user_id_event).await {
                                log_to_file(&format!("Using fetched user info: {}", user_info));
                                user_info
                            } else {
                                log_to_file(&format!("Failed to fetch user info, using user_id: {}", user_id_event));
                                user_id_event.to_string()
                            }
                        } else {
                            log_to_file(&format!("No user info available, using user_id_event: {}", user_id_event));
                            user_id_event.to_string()
                        };
                        log_to_file(&format!("Final user_name: {}", user_name));

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
