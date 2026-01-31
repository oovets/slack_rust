use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use crate::app::{ChatInfo, ChatSection};
use crate::config::Config;

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
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SlackReaction {
    pub name: String,
    pub count: u32,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SlackMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub ts: String,
    pub user: Option<String>,
    pub text: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub reply_count: Option<u32>,
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
}

#[derive(Deserialize)]
struct SocketModeConnectResponse {
    ok: bool,
    url: String,
}

impl SlackClient {
    pub async fn new(config: &Config) -> Result<Self> {
        let http = HttpClient::new();
        let token = config.token.clone();

        let client = Self {
            http,
            token,
            user_id: Arc::new(Mutex::new(None)),
            pending_updates: Arc::new(Mutex::new(Vec::new())),
            ws_handle: Arc::new(Mutex::new(None)),
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

        let handle = tokio::spawn(async move {
            if let Ok((mut ws_stream, _)) = connect_async(&response.url).await {
                while let Some(Ok(msg)) = ws_stream.next().await {
                    if let Message::Text(text) = msg {
                        if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&text) {
                            // Acknowledge envelope
                            if let Some(envelope_id) =
                                envelope.get("envelope_id").and_then(|v| v.as_str())
                            {
                                let ack = serde_json::json!({
                                    "envelope_id": envelope_id
                                });
                                let _ = ws_stream.send(Message::Text(ack.to_string())).await;
                            }

                            // Process event
                            if let Some(event_type) = envelope.get("type").and_then(|v| v.as_str())
                            {
                                if event_type == "events_api" {
                                    if let Some(event) =
                                        envelope.get("payload").and_then(|p| p.get("event"))
                                    {
                                        Self::process_event(
                                            event,
                                            &pending_updates,
                                            &http,
                                            &token,
                                            &user_id,
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                }
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
        if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
            match event_type {
                "message" => {
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

                        let my_id = user_id.lock().await.clone().unwrap_or_default();
                        let is_self = !my_id.is_empty() && user_id_event == my_id;

                        // Fetch user name
                        let user_name = if let Ok(user_info) =
                            Self::fetch_user_info(http, token, user_id_event).await
                        {
                            user_info
                        } else {
                            user_id_event.to_string()
                        };

                        pending_updates.lock().await.push(SlackUpdate::NewMessage {
                            channel_id: channel_id.to_string(),
                            user_name,
                            text: text.to_string(),
                            ts: ts.to_string(),
                            thread_ts,
                            is_bot,
                            is_self,
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
        let response: ConversationHistoryResponse = self
            .http
            .get(&format!(
                "https://slack.com/api/conversations.history?channel={}&limit={}",
                channel_id, limit
            ))
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        if !response.ok {
            return Err(anyhow!("Failed to fetch conversation history"));
        }

        Ok(response.messages)
    }

    pub async fn get_thread_replies(
        &self,
        channel_id: &str,
        thread_ts: &str,
        limit: usize,
    ) -> Result<Vec<SlackMessage>> {
        let response: ConversationHistoryResponse = self
            .http
            .get(&format!(
                "https://slack.com/api/conversations.replies?channel={}&ts={}&limit={}",
                channel_id, thread_ts, limit
            ))
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;

        if !response.ok {
            return Err(anyhow!("Failed to fetch thread replies"));
        }

        Ok(response.messages)
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
}
