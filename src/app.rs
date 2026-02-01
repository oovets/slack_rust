use anyhow::Result;
use chrono::{Local, TimeZone};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap},
    Frame,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::commands::CommandHandler;
use crate::config::Config;
use crate::formatting::{format_message_text, slack_emoji_to_unicode};
use crate::persistence::{Aliases, AppState, LayoutData};
use crate::slack::{SlackAttachment, SlackClient, SlackUpdate};
use crate::split_view::{PaneNode, SplitDirection};
use crate::utils::send_desktop_notification;
use crate::widgets::ChatPane;

pub struct App {
    pub config: Config,
    pub slack: SlackClient,
    pub my_user_id: String, // Current user's ID
    pub chats: Vec<ChatInfo>,
    pub selected_chat_idx: usize,
    pub panes: Vec<ChatPane>,
    pub focused_pane_idx: usize,
    pub pane_tree: PaneNode,
    pub input_history: Vec<String>,
    pub aliases: Aliases,
    pub focus_on_chat_list: bool,
    pub status_message: Option<String>,
    pub status_expire: Option<std::time::Instant>,
    pub pane_areas: std::collections::HashMap<usize, Rect>,
    pub chat_list_area: Option<Rect>,
    pub chat_list_scroll_offset: usize,
    pub pending_open_chat: bool,
    pub pending_refresh_chats: bool,
    pub pending_workspace_switch: Option<tokio::sync::oneshot::Receiver<Result<(SlackClient, String), String>>>,

    // Settings
    pub show_reactions: bool,
    pub show_notifications: bool,
    pub compact_mode: bool,
    pub show_emojis: bool,
    pub show_line_numbers: bool,
    pub show_timestamps: bool,
    pub show_chat_list: bool,
    pub show_user_colors: bool,
    pub show_borders: bool,
    pub user_name_cache: std::collections::HashMap<String, String>,
    pub needs_redraw: bool,
    pub last_terminal_size: (u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChatSection {
    Public = 0,
    Private = 1,
    Group = 2,
    DirectMessage = 3,
    Bot = 4,
}

impl ChatSection {
    pub fn label(&self) -> &'static str {
        match self {
            ChatSection::Public => "Public Channels",
            ChatSection::Private => "Private Channels",
            ChatSection::Group => "Group Chats",
            ChatSection::DirectMessage => "Messages",
            ChatSection::Bot => "Bots & Apps",
        }
    }
}

/// Generate a consistent color for a username using a hash function
fn username_color(username: &str) -> Color {
    // Use a palette of distinct, readable colors
    let colors = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::LightCyan,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::Rgb(255, 165, 0),  // Orange
        Color::Rgb(147, 112, 219), // Purple
        Color::Rgb(64, 224, 208),  // Turquoise
        Color::Rgb(255, 105, 180), // Hot Pink
        Color::Rgb(50, 205, 50),   // Lime Green
        Color::Rgb(255, 215, 0),   // Gold
    ];
    
    // Hash the username to get a consistent index
    let mut hasher = DefaultHasher::new();
    username.hash(&mut hasher);
    let hash = hasher.finish();
    
    // Use modulo to select a color from the palette
    colors[(hash as usize) % colors.len()]
}

#[derive(Clone)]
enum ChatListRow {
    Header(String),
    Chat(usize),
}

#[derive(Clone)]
pub struct ChatInfo {
    pub id: String,
    pub name: String,
    pub username: Option<String>,
    pub unread: u32,
    pub section: ChatSection,
}

fn forwarded_preview(attachments: &[SlackAttachment]) -> Option<String> {
    for att in attachments {
        // For URL previews and forwarded messages, show only title and author
        // to avoid excessive text that causes scroll issues
        let mut parts = Vec::new();
        
        // Add author name if present
        if let Some(author) = att.author_name.as_ref().filter(|a| !a.is_empty()) {
            parts.push(format!("@{}", author));
        }
        
        // Add title if present (main content for URL previews)
        if let Some(title) = att.title.as_ref().filter(|t| !t.is_empty()) {
            parts.push(title.clone());
        }
        
        // Only add short text snippets, ignore long URL preview descriptions
        if let Some(text) = att.text.as_ref().filter(|t| !t.is_empty()) {
            // Only include text if it's reasonably short (likely a forwarded message, not a URL preview)
            if text.len() <= 100 {
                parts.push(text.clone());
            }
        }
        
        if !parts.is_empty() {
            return Some(parts.join(" - "));
        }
        
        // Fallback to shortened fallback text
        if let Some(fallback) = att.fallback.as_ref().filter(|f| !f.is_empty()) {
            let short_fallback = if fallback.len() > 100 {
                format!("{}...", &fallback[..100])
            } else {
                fallback.clone()
            };
            return Some(short_fallback);
        }
    }
    None
}

impl App {
    /// Check if a message text contains a mention of the specified user ID
    fn message_mentions_user(text: &str, user_id: &str) -> bool {
        if user_id.is_empty() {
            return false;
        }
        
        // Look for <@USER_ID> or <@USER_ID|...>
        let pattern1 = format!("<@{}>", user_id);
        let pattern2 = format!("<@{}|", user_id);
        
        text.contains(&pattern1) || text.contains(&pattern2)
    }

    pub async fn new() -> Result<Self> {
        let config = Config::load()?;
        
        // Get the active workspace
        if config.workspaces.is_empty() {
            return Err(anyhow::anyhow!("No workspaces configured"));
        }
        // Ensure active_workspace is within bounds
        let active_idx = config.active_workspace.min(config.workspaces.len() - 1);
        let workspace = &config.workspaces[active_idx];
        
        let slack = SlackClient::new(&workspace.token, &workspace.app_token).await?;
        let my_user_id = slack.get_my_user_id().await?;

        // Start event listener
        slack.start_event_listener(workspace.app_token.clone()).await?;

        let app_state = AppState::load(&config).unwrap_or_else(|_| AppState {
            settings: crate::persistence::AppSettings::default(),
            aliases: Aliases::default(),
            layout: LayoutData::default(),
        });

        // Load initial chats
        let mut chats = slack.get_conversations().await.unwrap_or_else(|e| {
            eprintln!("Failed to load conversations: {e}");
            Vec::new()
        });
        chats.sort_by_key(|c| (c.section as u8, c.name.to_lowercase()));

        // Load pane tree
        let (pane_tree, required_indices) = if let Some(saved_tree) = app_state.layout.pane_tree {
            let indices = saved_tree.get_pane_indices();
            (saved_tree, indices)
        } else {
            let tree = PaneNode::new_single(0);
            let indices = tree.get_pane_indices();
            (tree, indices)
        };

        let max_required_idx = required_indices.iter().max().copied().unwrap_or(0);
        let total_panes_needed = (max_required_idx + 1)
            .max(app_state.layout.panes.len())
            .max(1);

        let mut panes: Vec<ChatPane> = Vec::new();
        for i in 0..total_panes_needed {
            if let Some(ps) = app_state.layout.panes.get(i) {
                let mut pane = ChatPane::new();
                pane.chat_id = ps.chat_id;
                pane.channel_id_str = ps.channel_id.clone();
                pane.chat_name = ps.chat_name.clone();
                pane.scroll_offset = ps.scroll_offset;
                panes.push(pane);
            } else {
                panes.push(ChatPane::new());
            }
        }

        let focused_pane_idx = if app_state.layout.focused_pane < panes.len() {
            app_state.layout.focused_pane
        } else {
            0
        };

        let app = Self {
            config,
            slack,
            my_user_id,
            chats,
            selected_chat_idx: 0,
            panes,
            focused_pane_idx,
            pane_tree,
            input_history: Vec::new(),
            aliases: app_state.aliases,
            focus_on_chat_list: true,
            status_message: None,
            status_expire: None,
            chat_list_area: None,
            chat_list_scroll_offset: 0,
            pending_open_chat: false,
            pending_refresh_chats: false,
            pending_workspace_switch: None,
            pane_areas: std::collections::HashMap::new(),
            show_reactions: app_state.settings.show_reactions,
            show_notifications: app_state.settings.show_notifications,
            compact_mode: app_state.settings.compact_mode,
            show_emojis: app_state.settings.show_emojis,
            show_line_numbers: app_state.settings.show_line_numbers,
            show_timestamps: app_state.settings.show_timestamps,
            show_chat_list: app_state.settings.show_chat_list,
            show_user_colors: app_state.settings.show_user_colors,
            show_borders: app_state.settings.show_borders,
            user_name_cache: std::collections::HashMap::new(),
            needs_redraw: true,
            last_terminal_size: (0, 0),
        };

        Ok(app)
    }
    
    /// Load chat history for all panes that have channels assigned
    pub async fn load_all_pane_histories(&mut self) -> Result<()> {
        // Collect channel IDs to load
        let channels_to_load: Vec<(usize, String)> = self.panes
            .iter()
            .enumerate()
            .filter_map(|(idx, pane)| {
                pane.channel_id_str.as_ref().map(|id| (idx, id.clone()))
            })
            .collect();

        for (pane_idx, channel_id) in channels_to_load {
            match self.slack.get_conversation_history(&channel_id, 500).await {
                Ok(messages) => {
                    // Collect unique user IDs and resolve names in batch
                    let mut name_cache: std::collections::HashMap<String, String> =
                        std::collections::HashMap::new();
                    for slack_msg in &messages {
                        if let Some(ref uid) = slack_msg.user {
                            if !name_cache.contains_key(uid) {
                                let name = self.slack.resolve_user_name(uid).await;
                                name_cache.insert(uid.clone(), name);
                            }
                        }
                    }

                    // Add messages to pane
                    let pane = &mut self.panes[pane_idx];
                    pane.msg_data.clear();
                    pane.invalidate_cache();
                    for slack_msg in messages.iter().rev() {
                        let user_id = slack_msg.user.clone().unwrap_or_default();
                        let sender_name = name_cache
                            .get(&user_id)
                            .cloned()
                            .unwrap_or_else(|| "Unknown".to_string());
                        let reactions: Vec<(String, u32)> = slack_msg
                            .reactions
                            .iter()
                            .map(|r| (r.name.clone(), r.count))
                            .collect();
                        let mentions_me = Self::message_mentions_user(&slack_msg.text, &self.my_user_id);
                        let msg_data = crate::widgets::MessageData {
                            sender_name,
                            text: slack_msg.text.clone(),
                            is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                            ts: slack_msg.ts.clone(),
                            reactions,
                            reply_count: slack_msg.reply_count.unwrap_or(0),
                            forwarded_text: forwarded_preview(&slack_msg.attachments),
                            mentions_me,
                        };
                        pane.msg_data.push(msg_data);
                    }
                    
                    // Auto-scroll to bottom
                    pane.scroll_offset = usize::MAX;
                }
                Err(e) => {
                    eprintln!("Failed to load messages for pane {}: {}", pane_idx, e);
                }
            }
        }
        
        // Sync user name cache
        self.user_name_cache = self.slack.get_user_name_cache().await;
        
        Ok(())
    }

    pub async fn process_slack_events(&mut self) -> Result<()> {
        let updates = self.slack.get_pending_updates().await;

        for update in updates {
            match update {
                SlackUpdate::NewMessage {
                    channel_id,
                    user_name,
                    text,
                    ts,
                    thread_ts,
                    is_bot,
                    is_self,
                    forwarded,
                    mentions_me,
                } => {
                    let is_thread_reply = matches!(thread_ts.as_ref(), Some(t) if t != &ts);
                    let root_thread_ts = thread_ts.clone().unwrap_or_else(|| ts.clone());

                    // Update panes showing this channel/thread
                    let mut seen_in_open_pane = false;
                    for pane in &mut self.panes {
                        if let Some(ref pane_channel_id) = pane.channel_id_str {
                            if *pane_channel_id == channel_id {
                                match &pane.thread_ts {
                                    Some(pane_thread) => {
                                        if let Some(msg_thread) = &thread_ts {
                                            if pane_thread == msg_thread {
                                                let msg_data = crate::widgets::MessageData {
                                                    sender_name: user_name.clone(),
                                                    text: text.clone(),
                                                    is_outgoing: is_self,
                                                    ts: ts.clone(),
                                                    reactions: Vec::new(),
                                                    reply_count: 0,
                                                    forwarded_text: forwarded.clone(),
                                                    mentions_me,
                                                };
                                                pane.msg_data.push(msg_data);
                                                pane.invalidate_cache();
                                                pane.scroll_offset = usize::MAX;
                                                seen_in_open_pane = true;
                                            }
                                        }
                                    }
                                    None => {
                                        if is_thread_reply {
                                            if let Some(parent) = pane
                                                .msg_data
                                                .iter_mut()
                                                .find(|m| m.ts == root_thread_ts)
                                            {
                                                parent.reply_count =
                                                    parent.reply_count.saturating_add(1);
                                            }
                                        } else {
                                            let msg_data = crate::widgets::MessageData {
                                                sender_name: user_name.clone(),
                                                text: text.clone(),
                                                is_outgoing: is_self,
                                                ts: ts.clone(),
                                                reactions: Vec::new(),
                                                reply_count: 0,
                                                forwarded_text: forwarded.clone(),
                                                mentions_me,
                                            };
                                            pane.msg_data.push(msg_data);
                                            pane.invalidate_cache();
                                            pane.scroll_offset = usize::MAX;
                                            seen_in_open_pane = true;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Mark channel as unread if it's not currently visible
                    if let Some(chat) = self.chats.iter_mut().find(|c| c.id == channel_id) {
                        if seen_in_open_pane {
                            chat.unread = 0;
                        } else if !is_self {
                            chat.unread = chat.unread.saturating_add(1);
                        }
                    }

                    self.needs_redraw = true;

                    // Send notification only when mentioned
                    if self.show_notifications && !is_bot && !is_self && mentions_me {
                        let channel_name = self
                            .chats
                            .iter()
                            .find(|c| c.id == channel_id)
                            .map(|c| c.name.clone())
                            .or_else(|| {
                                self.panes
                                    .iter()
                                    .find(|p| {
                                        p.channel_id_str.as_deref() == Some(channel_id.as_str())
                                    })
                                    .map(|p| p.chat_name.clone())
                            })
                            .unwrap_or_else(|| channel_id.clone());
                        let title = channel_name;
                        let _ = send_desktop_notification(
                            &format!("Slack: {} - You were mentioned!", title),
                            &format!("{}: {}", user_name, text),
                        );
                    }
                }
                SlackUpdate::UserTyping {
                    channel_id,
                    user_name,
                } => {
                    for pane in &mut self.panes {
                        if let Some(ref pane_channel_id) = pane.channel_id_str {
                            if pane_channel_id == &channel_id {
                                pane.show_typing_indicator(&user_name);
                                break;
                            }
                        }
                    }
                    self.needs_redraw = true;
                }
            }
        }

        Ok(())
    }

    pub async fn refresh_chats(&mut self) -> Result<()> {
        self.chats = self.slack.get_conversations().await?;
        self.chats
            .sort_by_key(|c| (c.section as u8, c.name.to_lowercase()));
        if self.selected_chat_idx >= self.chats.len() {
            self.selected_chat_idx = self.chats.len().saturating_sub(1);
        }
        self.set_status("Chats refreshed");
        Ok(())
    }

    pub async fn open_selected_chat(&mut self) -> Result<()> {
        self.ensure_valid_pane_idx();
        if self.selected_chat_idx >= self.chats.len() {
            return Ok(());
        }

        let chat = self.chats[self.selected_chat_idx].clone();
        let pane = &mut self.panes[self.focused_pane_idx];

        // Use string channel ID (Slack IDs are not numeric)
        pane.chat_id = None;
        pane.channel_id_str = Some(chat.id.clone());
        pane.chat_name = chat.name.clone();
        pane.username = chat.username.clone();
        pane.thread_ts = None;
        pane.msg_data.clear();
        pane.invalidate_cache();

        // Clear unread counter when opening the chat
        if let Some(chat_info) = self.chats.get_mut(self.selected_chat_idx) {
            chat_info.unread = 0;
        }

        // Load messages
        match self.slack.get_conversation_history(&chat.id, 500).await {
            Ok(messages) => {
                // Collect unique user IDs and resolve names in batch
                let mut name_cache: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for slack_msg in &messages {
                    if let Some(ref uid) = slack_msg.user {
                        if !name_cache.contains_key(uid) {
                            let name = self.slack.resolve_user_name(uid).await;
                            name_cache.insert(uid.clone(), name);
                        }
                    }
                }

                for slack_msg in messages.iter().rev() {
                    let user_id = slack_msg.user.clone().unwrap_or_default();
                    let sender_name = name_cache
                        .get(&user_id)
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_string());
                    let reactions: Vec<(String, u32)> = slack_msg
                        .reactions
                        .iter()
                        .map(|r| (r.name.clone(), r.count))
                        .collect();
                    let mentions_me = Self::message_mentions_user(&slack_msg.text, &self.my_user_id);
                    let msg_data = crate::widgets::MessageData {
                        sender_name,
                        text: slack_msg.text.clone(),
                        is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                        ts: slack_msg.ts.clone(),
                        reactions,
                        reply_count: slack_msg.reply_count.unwrap_or(0),
                        forwarded_text: forwarded_preview(&slack_msg.attachments),
                        mentions_me,
                    };
                    pane.msg_data.push(msg_data);
                }
            }
            Err(e) => {
                self.set_status(&format!("Failed to load messages: {}", e));
            }
        }

        // Sync user name cache
        self.user_name_cache = self.slack.get_user_name_cache().await;

        // Auto-scroll to bottom
        self.panes[self.focused_pane_idx].scroll_offset = usize::MAX;
        self.focus_on_chat_list = false;
        Ok(())
    }

    pub async fn open_thread(
        &mut self,
        channel_id_str: &str,
        thread_ts: &str,
        parent_user: &str,
    ) -> Result<()> {
        // Create new pane for thread
        let new_idx = self.panes.len();
        let mut thread_pane = ChatPane::new();
        thread_pane.channel_id_str = Some(channel_id_str.to_string());
        thread_pane.thread_ts = Some(thread_ts.to_string());
        thread_pane.chat_name = format!("Thread: {}", parent_user);
        self.panes.push(thread_pane);

        // Split current pane vertically, thread takes 1/3
        self.pane_tree
            .split_with_ratio(SplitDirection::Vertical, new_idx, 33);

        // Load thread replies
        match self
            .slack
            .get_thread_replies(channel_id_str, thread_ts, 100)
            .await
        {
            Ok(messages) => {
                let mut name_cache: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for slack_msg in &messages {
                    if let Some(ref uid) = slack_msg.user {
                        if !name_cache.contains_key(uid) {
                            let name = self.slack.resolve_user_name(uid).await;
                            name_cache.insert(uid.clone(), name);
                        }
                    }
                }

                let pane = &mut self.panes[new_idx];
                for slack_msg in &messages {
                    let user_id = slack_msg.user.clone().unwrap_or_default();
                    let sender_name = name_cache
                        .get(&user_id)
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_string());
                    let reactions: Vec<(String, u32)> = slack_msg
                        .reactions
                        .iter()
                        .map(|r| (r.name.clone(), r.count))
                        .collect();
                    let mentions_me = Self::message_mentions_user(&slack_msg.text, &self.my_user_id);
                    let msg_data = crate::widgets::MessageData {
                        sender_name,
                        text: slack_msg.text.clone(),
                        is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                        ts: slack_msg.ts.clone(),
                        reactions,
                        reply_count: 0,
                        forwarded_text: forwarded_preview(&slack_msg.attachments),
                        mentions_me,
                    };
                    pane.msg_data.push(msg_data);
                }
            }
            Err(e) => {
                self.set_status(&format!("Failed to load thread: {}", e));
            }
        }

        // Sync user name cache
        self.user_name_cache = self.slack.get_user_name_cache().await;

        // Auto-scroll to bottom
        self.panes[new_idx].scroll_offset = usize::MAX;
        self.focused_pane_idx = new_idx;
        self.focus_on_chat_list = false;
        Ok(())
    }

    pub async fn send_message(&mut self) -> Result<()> {
        self.ensure_valid_pane_idx();
        let pane_idx = self.focused_pane_idx;
        let input = self.panes[pane_idx].input_buffer.trim().to_string();

        if input.is_empty() {
            return Ok(());
        }

        // Check if it's a command
        if input.starts_with('/') {
            let mut handler = CommandHandler::new();
            handler.handle_command(self, &input).await?;
            // After handle_command, pane_idx might be invalid if workspace was switched
            // Use focused_pane_idx which is always kept valid by ensure_valid_pane_idx
            self.ensure_valid_pane_idx();
            let idx = self.focused_pane_idx;
            self.panes[idx].input_buffer.clear();
            self.panes[idx].input_cursor = 0;
            self.panes[idx].tab_complete_state = None;
            return Ok(());
        }

        let channel_id_str = self.panes[pane_idx].channel_id_str.clone();
        let thread_ts = self.panes[pane_idx].thread_ts.clone();
        if let Some(channel_id) = channel_id_str {
            match self
                .slack
                .send_message(&channel_id, &input, thread_ts.as_deref())
                .await
            {
                Ok(_) => {
                    self.input_history.push(input.clone());
                    self.panes[pane_idx].input_buffer.clear();
                    self.panes[pane_idx].input_cursor = 0;
                    self.panes[pane_idx].tab_complete_state = None;
                }
                Err(e) => {
                    self.set_status(&format!("Failed to send: {}", e));
                }
            }
        }

        Ok(())
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let has_status = self.status_message.is_some();
        let main_constraints = if has_status {
            vec![Constraint::Min(0), Constraint::Length(1)]
        } else {
            vec![Constraint::Min(0)]
        };

        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints(main_constraints)
            .split(f.area());

        let (chat_area, pane_area) = if self.show_chat_list {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
                .split(outer[0]);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, outer[0])
        };

        if let Some(area) = chat_area {
            self.chat_list_area = Some(area);
            self.draw_chat_list(f, area);
        } else {
            self.chat_list_area = None;
        }

        // Draw panes
        let render_fn = |f: &mut Frame, area: Rect, pane: &ChatPane, is_focused: bool| {
            self.draw_chat_pane_impl(f, area, pane, is_focused);
        };

        let mut pane_areas = std::collections::HashMap::new();
        self.pane_tree.render(
            f,
            pane_area,
            &self.panes,
            self.focused_pane_idx,
            &render_fn,
            &mut pane_areas,
        );
        self.pane_areas = pane_areas;

        // Draw status bar
        if has_status {
            let status = Paragraph::new(self.status_message.as_ref().unwrap().clone())
                .style(Style::default().bg(Color::DarkGray).fg(Color::White))
                .block(Block::default());
            f.render_widget(status, outer[1]);
        }
    }

    /// Build the display rows for the chat list with a "New" section on top.
    fn build_chat_list_rows(&self) -> Vec<ChatListRow> {
        let sections = [
            ChatSection::Public,
            ChatSection::Private,
            ChatSection::Group,
            ChatSection::DirectMessage,
            ChatSection::Bot,
        ];

        let mut rows: Vec<ChatListRow> = Vec::new();

        // New section (unread > 0)
        let new_chats: Vec<usize> = self
            .chats
            .iter()
            .enumerate()
            .filter(|(_, c)| c.unread > 0)
            .map(|(i, _)| i)
            .collect();
        if !new_chats.is_empty() {
            rows.push(ChatListRow::Header("New".to_string()));
            for idx in new_chats {
                rows.push(ChatListRow::Chat(idx));
            }
        }

        // Regular sections with only read chats
        for section in &sections {
            let section_chats: Vec<usize> = self
                .chats
                .iter()
                .enumerate()
                .filter(|(_, c)| c.section == *section && c.unread == 0)
                .map(|(i, _)| i)
                .collect();

            if section_chats.is_empty() {
                continue;
            }

            rows.push(ChatListRow::Header(section.label().to_string()));
            for idx in section_chats {
                rows.push(ChatListRow::Chat(idx));
            }
        }
        rows
    }

    /// Find the display row index for a given chat index.
    fn chat_idx_to_row(&self, rows: &[ChatListRow], chat_idx: usize) -> usize {
        rows.iter()
            .position(|r| matches!(r, ChatListRow::Chat(idx) if *idx == chat_idx))
            .unwrap_or(0)
    }

    /// Find the chat index from a display row click.
    fn row_to_chat_idx(rows: &[ChatListRow], row: usize) -> Option<usize> {
        rows.get(row).and_then(|r| match r {
            ChatListRow::Chat(idx) => Some(*idx),
            _ => None,
        })
    }

    fn draw_chat_list(&mut self, f: &mut Frame, area: Rect) {
        let visible_height = area.height.saturating_sub(2) as usize;
        if visible_height == 0 {
            return;
        }

        let rows = self.build_chat_list_rows();
        let selected_row = self.chat_idx_to_row(&rows, self.selected_chat_idx);

        // Ensure scroll offset keeps selected row visible
        if selected_row < self.chat_list_scroll_offset {
            self.chat_list_scroll_offset = selected_row;
        } else if selected_row >= self.chat_list_scroll_offset + visible_height {
            self.chat_list_scroll_offset = selected_row + 1 - visible_height;
        }

        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .skip(self.chat_list_scroll_offset)
            .take(visible_height)
            .map(|(_, row)| match row {
                ChatListRow::Header(label) => ListItem::new(Line::from(Span::styled(
                    format!("-- {} --", label),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ))),
                ChatListRow::Chat(chat_idx) => {
                    let chat = &self.chats[*chat_idx];
                    let mut style = if *chat_idx == self.selected_chat_idx {
                        Style::default().bg(Color::Blue).fg(Color::White)
                    } else if chat.unread > 0 {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    };

                    if chat.unread > 0 && *chat_idx == self.selected_chat_idx {
                        style = style.add_modifier(Modifier::BOLD);
                    }

                    let unread_marker = if chat.unread > 0 {
                        format!(" ({})", chat.unread)
                    } else {
                        String::new()
                    };

                    let mut spans = vec![];
                    if chat.unread > 0 {
                        spans.push(Span::styled(
                            "! ",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    spans.push(Span::raw(format!("{}{}", chat.name, unread_marker)));

                    ListItem::new(Line::from(spans)).style(style)
                }
            })
            .collect();

        let list_block = if self.show_borders {
            Block::default()
                .borders(Borders::ALL)
                .title(if self.focus_on_chat_list {
                    "Channels [FOCUSED]"
                } else {
                    "Channels"
                })
                .border_style(if self.focus_on_chat_list {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                })
        } else {
            Block::default()
        };
        let list = List::new(items).block(list_block);

        f.render_widget(list, area);
    }

    fn draw_chat_pane_impl(&self, f: &mut Frame, area: Rect, pane: &ChatPane, is_focused: bool) {
        let has_reply_preview = pane.reply_preview.is_some();
        let header_height = if !self.show_borders { 2 } else if self.compact_mode { 2 } else { 3 };
        let input_height: u16 = 5; // top margin + 3 lines + bottom margin
        let constraints = if has_reply_preview {
            vec![
                Constraint::Length(header_height),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(input_height),
            ]
        } else {
            vec![
                Constraint::Length(header_height),
                Constraint::Min(0),
                Constraint::Length(input_height),
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Header
        let header_style = if is_focused {
            if self.focus_on_chat_list {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            }
        } else {
            Style::default().fg(Color::Cyan)
        };

        let mut header_text = String::new();
        if is_focused && self.focus_on_chat_list {
            header_text.push_str("[TARGET] ");
        }
        header_text.push_str(&pane.header_text());

        let header = Paragraph::new(header_text)
            .block(if self.show_borders {
                Block::default().borders(Borders::ALL)
            } else {
                Block::default()
            })
            .style(header_style);
        f.render_widget(header, chunks[0]);

        let messages_block = if self.show_borders {
            Block::default().borders(Borders::ALL).title("Messages")
        } else {
            Block::default().padding(Padding::left(2))
        };
        let msg_inner = messages_block.inner(chunks[1]);
        let msg_width = msg_inner.width as usize;
        let msg_area_height = msg_inner.height as usize;

        let show_emojis = self.show_emojis;
        let show_reactions = self.show_reactions;
        let show_line_numbers = self.show_line_numbers;
        let show_timestamps = self.show_timestamps;
        let show_user_colors = self.show_user_colors;
        let user_cache = &self.user_name_cache;
        let resolve_user = |id: &str| -> String {
            user_cache
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        };
        let format_ts = |ts: &str| -> Option<String> {
            if !show_timestamps {
                return None;
            }
            let secs: i64 = ts.split('.').next()?.parse().ok()?;
            let dt = Local.timestamp_opt(secs, 0).single()?;
            Some(dt.format("%Y-%m-%d %H:%M").to_string())
        };

        // Messages with emojis, reactions, and thread indicators
        let mut message_lines: Vec<Line> = Vec::new();
        for (idx, msg) in pane.msg_data.iter().enumerate() {
            let name_style = if msg.is_outgoing {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            };

            let formatted_text = format_message_text(&msg.text, show_emojis, &resolve_user);

            let mut prefix_spans = Vec::new();

            // Add highlight indicator if message mentions the user
            if msg.mentions_me {
                prefix_spans.push(Span::styled(
                    "@ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            if show_line_numbers {
                prefix_spans.push(Span::styled(
                    format!("#{} ", idx + 1),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            if let Some(ts_fmt) = format_ts(&msg.ts) {
                prefix_spans.push(Span::styled(
                    format!("[{}] ", ts_fmt),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            // Use color-coded username for better visual distinction
            let username_style = if msg.is_outgoing {
                name_style  // Keep own messages with original style
            } else if show_user_colors {
                Style::default()
                    .fg(username_color(&msg.sender_name))
                    .add_modifier(Modifier::BOLD)
            } else {
                name_style  // Use default style if colors are disabled
            };
            prefix_spans.push(Span::styled(
                format!("{}: ", msg.sender_name),
                username_style,
            ));

            let mut content_spans = Vec::new();
            content_spans.push(Span::raw(formatted_text));

            // Thread reply indicator
            if msg.reply_count > 0 {
                content_spans.push(Span::styled(
                    format!(" [{} replies]", msg.reply_count),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            // Reactions inline
            if show_reactions && !msg.reactions.is_empty() {
                let reaction_str: String = msg
                    .reactions
                    .iter()
                    .map(|(name, count)| {
                        let emoji = slack_emoji_to_unicode(name);
                        if *count > 1 {
                            format!("{}x{}", count, emoji)
                        } else {
                            emoji
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                content_spans.push(Span::styled(
                    format!("  {}", reaction_str),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            let prefix_width = spans_width(&prefix_spans);
            let indent = " ".repeat(prefix_width);
            let indent_width = UnicodeWidthStr::width(indent.as_str());
            let first_width = msg_width.saturating_sub(prefix_width);
            let rest_width = msg_width.saturating_sub(indent_width);
            let mut wrapped =
                wrap_spans_hanging(&content_spans, first_width, rest_width, indent.as_str());
            if wrapped.is_empty() {
                wrapped.push(Vec::new());
            }
            let mut first_line = prefix_spans;
            first_line.extend(wrapped.remove(0));
            message_lines.push(Line::from(first_line));
            for line in wrapped {
                message_lines.push(Line::from(line));
            }

            // Show quoted/forwarded message as indented block (max 3 lines)
            if let Some(ref fwd) = msg.forwarded_text {
                let quote_style = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC);
                let quote_prefix = vec![Span::styled("│ ", quote_style)];
                let quote_prefix_width = spans_width(&quote_prefix);
                let quote_indent = format!("{}{}", "│ ", " ".repeat(quote_prefix_width.saturating_sub(2)));
                let quote_first_width = msg_width.saturating_sub(quote_prefix_width);
                let quote_rest_width =
                    msg_width.saturating_sub(UnicodeWidthStr::width(quote_indent.as_str()));
                let quote_spans = vec![Span::styled(fwd.as_str(), quote_style)];
                let mut quote_lines = wrap_spans_hanging(
                    &quote_spans,
                    quote_first_width,
                    quote_rest_width,
                    quote_indent.as_str(),
                );
                if quote_lines.is_empty() {
                    quote_lines.push(Vec::new());
                }
                if quote_lines.len() > 3 {
                    quote_lines.truncate(3);
                    quote_lines.push(vec![Span::styled("│ ...", quote_style)]);
                }
                let mut first_line = quote_prefix;
                first_line.extend(quote_lines.remove(0));
                message_lines.push(Line::from(first_line));
                for line in quote_lines {
                    message_lines.push(Line::from(line));
                }
            }
        }

        let messages = Paragraph::new(message_lines)
            .block(messages_block);

        // Use ratatui's own line_count with inner width for accurate wrapping
        // line_count adds vertical space back, so subtract it to get content lines only
        let vertical_space = if self.show_borders { 2u16 } else { 0u16 };
        let total_wrapped_lines = messages.line_count(msg_inner.width)
            .saturating_sub(vertical_space as usize);
        let max_scroll = total_wrapped_lines.saturating_sub(msg_area_height);
        let scroll_offset = pane.scroll_offset.min(max_scroll);

        let messages = messages.scroll((scroll_offset as u16, 0));

        f.render_widget(messages, chunks[1]);

        // Reply preview if present
        if has_reply_preview {
            if let Some(ref preview) = pane.reply_preview {
                let reply_bar =
                    Paragraph::new(preview.as_str()).style(Style::default().fg(Color::Yellow));
                f.render_widget(reply_bar, chunks[2]);
            }
        }

        // Input
        let input_chunk = if has_reply_preview {
            chunks[3]
        } else {
            chunks[2]
        };
        let input_style = if is_focused && !self.focus_on_chat_list {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Gray)
        };

        // Render input with blank line above/below
        let top_margin: u16 = 1;
        let bottom_margin: u16 = 1;
        let input_inner = Rect {
            x: input_chunk.x,
            y: input_chunk.y + top_margin,
            width: input_chunk.width,
            height: input_chunk.height.saturating_sub(top_margin + bottom_margin),
        };
        let (cursor_line, cursor_col) = cursor_visual_pos(
            pane.input_buffer.as_str(),
            pane.input_cursor,
            input_inner.width as usize,
        );
        let input_scroll = if input_inner.height > 0 {
            cursor_line.saturating_sub(input_inner.height as usize - 1)
        } else {
            0
        };

        let input = Paragraph::new(pane.input_buffer.as_str())
            .style(input_style)
            .wrap(Wrap { trim: false })
            .scroll((input_scroll as u16, 0));

        f.render_widget(input, input_inner);

        // Set cursor position only when input is focused
        if is_focused && !self.focus_on_chat_list {
            let cursor_y = input_inner.y + cursor_line.saturating_sub(input_scroll) as u16;
            let cursor_x = input_inner.x + cursor_col as u16;
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }

    pub fn set_status(&mut self, message: &str) {
        self.status_message = Some(message.to_string());
        self.status_expire = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        self.needs_redraw = true;
    }

    pub fn save_state(&self) -> Result<()> {
        let state = AppState {
            settings: crate::persistence::AppSettings {
                show_reactions: self.show_reactions,
                show_notifications: self.show_notifications,
                compact_mode: self.compact_mode,
                show_emojis: self.show_emojis,
                show_line_numbers: self.show_line_numbers,
                show_timestamps: self.show_timestamps,
                show_chat_list: self.show_chat_list,
                show_user_colors: self.show_user_colors,
                show_borders: self.show_borders,
            },
            aliases: self.aliases.clone(),
            layout: LayoutData {
                panes: self
                    .panes
                    .iter()
                    .map(|p| crate::persistence::PaneState {
                        chat_id: p.chat_id,
                        channel_id: p.channel_id_str.clone(),
                        chat_name: p.chat_name.clone(),
                        scroll_offset: p.scroll_offset,
                        filter_type: None,
                        filter_value: None,
                    })
                    .collect(),
                focused_pane: self.focused_pane_idx,
                pane_tree: Some(self.pane_tree.clone()),
            },
        };

        state.save(&self.config)
    }

    // Navigation methods
    pub fn select_next_chat(&mut self) {
        if !self.chats.is_empty() {
            self.selected_chat_idx = (self.selected_chat_idx + 1) % self.chats.len();
        }
    }

    pub fn select_previous_chat(&mut self) {
        if !self.chats.is_empty() {
            self.selected_chat_idx = if self.selected_chat_idx == 0 {
                self.chats.len() - 1
            } else {
                self.selected_chat_idx - 1
            };
        }
    }

    pub fn next_pane(&mut self) {
        if self.focus_on_chat_list {
            self.focus_on_chat_list = false;
        } else if self.panes.len() > 1 {
            self.focused_pane_idx = (self.focused_pane_idx + 1) % self.panes.len();
        }
    }

    pub fn scroll_up(&mut self) {
        self.ensure_valid_pane_idx();
        self.panes[self.focused_pane_idx].scroll_up();
    }

    pub fn scroll_down(&mut self) {
        self.ensure_valid_pane_idx();
        self.panes[self.focused_pane_idx].scroll_down();
    }

    pub fn page_up(&mut self) {
        for _ in 0..10 {
            self.panes[self.focused_pane_idx].scroll_up();
        }
    }

    pub fn page_down(&mut self) {
        for _ in 0..10 {
            self.panes[self.focused_pane_idx].scroll_down();
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.panes[self.focused_pane_idx].scroll_offset = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.panes[self.focused_pane_idx].scroll_offset = usize::MAX;
    }

    pub fn input_char(&mut self, c: char) {
        self.ensure_valid_pane_idx();
        let pane = &mut self.panes[self.focused_pane_idx];
        pane.input_buffer.insert(pane.input_cursor, c);
        pane.input_cursor += c.len_utf8();
        pane.tab_complete_state = None;
    }

    pub fn backspace(&mut self) {
        self.ensure_valid_pane_idx();
        let pane = &mut self.panes[self.focused_pane_idx];
        if pane.input_cursor == 0 {
            return;
        }
        let prev = prev_char_boundary(&pane.input_buffer, pane.input_cursor);
        pane.input_buffer.drain(prev..pane.input_cursor);
        pane.input_cursor = prev;
        pane.tab_complete_state = None;
    }

    pub fn delete_forward(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        if pane.input_cursor >= pane.input_buffer.len() {
            return;
        }
        let next = next_char_boundary(&pane.input_buffer, pane.input_cursor);
        pane.input_buffer.drain(pane.input_cursor..next);
        pane.tab_complete_state = None;
    }

    pub fn input_newline(&mut self) {
        self.ensure_valid_pane_idx();
        let pane = &mut self.panes[self.focused_pane_idx];
        pane.input_buffer.insert(pane.input_cursor, '\n');
        pane.input_cursor += 1;
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_left(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        if pane.input_cursor == 0 {
            return;
        }
        pane.input_cursor = prev_char_boundary(&pane.input_buffer, pane.input_cursor);
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_right(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        if pane.input_cursor >= pane.input_buffer.len() {
            return;
        }
        pane.input_cursor = next_char_boundary(&pane.input_buffer, pane.input_cursor);
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_home(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        let (line_start, _) = line_bounds(&pane.input_buffer, pane.input_cursor);
        pane.input_cursor = line_start;
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_end(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        let (_, line_end) = line_bounds(&pane.input_buffer, pane.input_cursor);
        pane.input_cursor = line_end;
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_up(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        let (line_start, _) = line_bounds(&pane.input_buffer, pane.input_cursor);
        if line_start == 0 {
            return;
        }
        let target_col = column_in_line(&pane.input_buffer, line_start, pane.input_cursor);
        let prev_line_end = line_start.saturating_sub(1);
        let prev_line_start = pane.input_buffer[..prev_line_end]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let new_cursor = index_from_column(
            &pane.input_buffer,
            prev_line_start,
            prev_line_end,
            target_col,
        );
        pane.input_cursor = new_cursor.min(pane.input_buffer.len());
        pane.tab_complete_state = None;
    }

    pub fn move_cursor_down(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        let (line_start, line_end) = line_bounds(&pane.input_buffer, pane.input_cursor);
        if line_end >= pane.input_buffer.len() {
            return;
        }
        let target_col = column_in_line(&pane.input_buffer, line_start, pane.input_cursor);
        let next_line_start = line_end + 1;
        let next_line_end = pane.input_buffer[next_line_start..]
            .find('\n')
            .map(|idx| next_line_start + idx)
            .unwrap_or_else(|| pane.input_buffer.len());
        let new_cursor = index_from_column(
            &pane.input_buffer,
            next_line_start,
            next_line_end,
            target_col,
        );
        pane.input_cursor = new_cursor.min(pane.input_buffer.len());
        pane.tab_complete_state = None;
    }

    pub fn tab_complete(&mut self) {
        use crate::widgets::TabCompleteState;

        self.ensure_valid_pane_idx();
        let pane = &mut self.panes[self.focused_pane_idx];

        if let Some(ref mut state) = pane.tab_complete_state {
            // Cycle to next candidate
            if state.candidates.is_empty() {
                return;
            }
            state.index = (state.index + 1) % state.candidates.len();
            let replacement = &state.candidates[state.index];
            
            if state.before.starts_with('/') {
                // Command completion
                pane.input_buffer = format!("/{} {}", replacement, state.after);
                pane.input_cursor = replacement.len() + 2;
            } else {
                // User mention completion
                pane.input_buffer = format!("{}@{} {}", state.before, replacement, state.after);
                pane.input_cursor = state.before.len() + replacement.len() + 2;
            }
        } else {
            let input = &pane.input_buffer;
            let cursor = pane.input_cursor.min(input.len());
            let before_cursor = &input[..cursor];
            
            // Check if completing a command
            if before_cursor.starts_with('/') && !before_cursor.contains(' ') {
                let prefix = &before_cursor[1..];
                let prefix_lower = prefix.to_lowercase();
                
                // All available commands
                let commands = vec![
                    "thread", "t", "react", "filter", "alias", "unalias",
                    "workspace", "ws", "leave", "help", "h"
                ];
                
                let mut candidates: Vec<String> = commands
                    .into_iter()
                    .filter(|cmd| cmd.starts_with(&prefix_lower))
                    .map(|s| s.to_string())
                    .collect();
                    
                if candidates.is_empty() {
                    return;
                }
                
                candidates.sort();
                let after = input[cursor..].to_string();
                let replacement = &candidates[0];
                
                pane.input_buffer = format!("/{} {}", replacement, after);
                pane.input_cursor = replacement.len() + 2;
                
                pane.tab_complete_state = Some(TabCompleteState {
                    before: "/".to_string(),
                    after,
                    candidates,
                    index: 0,
                });
                return;
            }
            
            // Find @prefix at cursor for user mentions
            let at_pos = before_cursor.rfind('@');
            if at_pos.is_none() {
                return;
            }
            let at_pos = at_pos.unwrap();
            let prefix = &before_cursor[at_pos + 1..];
            // Don't complete empty @ or if there's a space after @
            if prefix.is_empty() || prefix.contains(' ') {
                return;
            }
            let prefix_lower = prefix.to_lowercase();

            // Find matching names from cache
            let mut candidates: Vec<String> = self
                .user_name_cache
                .values()
                .filter(|name| name.to_lowercase().starts_with(&prefix_lower))
                .cloned()
                .collect();
            candidates.sort();
            candidates.dedup();

            if candidates.is_empty() {
                return;
            }

            let replacement = &candidates[0];
            let before = input[..at_pos].to_string();
            let after = input[cursor..].to_string();
            pane.input_buffer = format!("{}@{} {}", before, replacement, after);
            pane.input_cursor = before.len() + replacement.len() + 2;

            pane.tab_complete_state = Some(TabCompleteState {
                before,
                after,
                candidates,
                index: 0,
            });
        }
    }

    pub fn cancel_reply(&mut self) {
        let pane = &mut self.panes[self.focused_pane_idx];
        pane.reply_to_message = None;
        pane.hide_reply_preview();
    }

    // Split management
    pub fn split_vertical(&mut self) {
        let new_idx = self.panes.len();
        self.panes.push(ChatPane::new());
        // Split the focused pane, not the root
        if !self.pane_tree.split_pane(self.focused_pane_idx, SplitDirection::Vertical, new_idx) {
            // Fallback: split at root if focused pane not found
            self.pane_tree.split(SplitDirection::Vertical, new_idx);
        }
        self.focused_pane_idx = new_idx; // Focus the new pane
    }

    pub fn split_horizontal(&mut self) {
        let new_idx = self.panes.len();
        self.panes.push(ChatPane::new());
        // Split the focused pane, not the root
        if !self.pane_tree.split_pane(self.focused_pane_idx, SplitDirection::Horizontal, new_idx) {
            // Fallback: split at root if focused pane not found
            self.pane_tree.split(SplitDirection::Horizontal, new_idx);
        }
        self.focused_pane_idx = new_idx; // Focus the new pane
    }

    pub fn toggle_split_direction(&mut self) {
        self.pane_tree.toggle_direction();
    }

    pub fn close_pane(&mut self) {
        if self.panes.len() <= 1 {
            self.set_status("Cannot close the last pane");
            return;
        }
        
        let pane_idx = self.focused_pane_idx;
        
        // Get all pane indices before closing
        let all_indices = self.pane_tree.get_pane_indices();
        
        // Find a new pane to focus on (prefer next pane, or previous if we're at the end)
        let current_pos = all_indices.iter().position(|&idx| idx == pane_idx).unwrap_or(0);
        let new_focus_idx = if current_pos + 1 < all_indices.len() {
            all_indices[current_pos + 1]
        } else if current_pos > 0 {
            all_indices[current_pos - 1]
        } else {
            0
        };
        
        // Remove the pane from the tree
        self.pane_tree.close_pane(pane_idx);
        
        // Remove the pane from the array
        self.panes.remove(pane_idx);
        
        // Reindex all pane indices in the tree (shift down indices > pane_idx)
        self.pane_tree.reindex_after_removal(pane_idx);
        
        // Update focused pane index (adjust if it was after the removed pane)
        self.focused_pane_idx = if new_focus_idx > pane_idx {
            new_focus_idx - 1
        } else {
            new_focus_idx
        };
        
        // Ensure focused index is valid
        if self.focused_pane_idx >= self.panes.len() {
            self.focused_pane_idx = self.panes.len().saturating_sub(1);
        }
    }

    pub fn clear_pane(&mut self) {
        self.panes[self.focused_pane_idx].clear();
    }

    // Toggle settings
    pub fn toggle_chat_list(&mut self) {
        self.show_chat_list = !self.show_chat_list;
        self.needs_redraw = true;
    }

    pub fn toggle_reactions(&mut self) {
        self.show_reactions = !self.show_reactions;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_emojis(&mut self) {
        self.show_emojis = !self.show_emojis;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_timestamps(&mut self) {
        self.show_timestamps = !self.show_timestamps;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_compact_mode(&mut self) {
        self.compact_mode = !self.compact_mode;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_user_colors(&mut self) {
        self.show_user_colors = !self.show_user_colors;
        for pane in &mut self.panes {
            pane.invalidate_cache();
        }
        self.needs_redraw = true;
    }

    pub fn toggle_borders(&mut self) {
        self.show_borders = !self.show_borders;
        self.needs_redraw = true;
    }

    pub fn handle_mouse_click(&mut self, x: u16, y: u16) {
        // Check if click is in chat list
        if let Some(area) = self.chat_list_area {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                self.focus_on_chat_list = true;
                // Calculate which chat was clicked (accounting for scroll offset and border)
                let border_offset = if self.show_borders { 1 } else { 0 };
                let relative_y = y.saturating_sub(area.y + border_offset);
                let row_idx = relative_y as usize + self.chat_list_scroll_offset;
                let rows = self.build_chat_list_rows();
                if let Some(chat_idx) = Self::row_to_chat_idx(&rows, row_idx) {
                    self.selected_chat_idx = chat_idx;
                    self.pending_open_chat = true;
                }
                return;
            }
        }

        // Check if click is in a pane
        for (idx, area) in &self.pane_areas {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                self.focused_pane_idx = *idx;
                self.focus_on_chat_list = false;
                return;
            }
        }
    }

    pub fn switch_workspace(&mut self, workspace_idx: usize) {
        if workspace_idx >= self.config.workspaces.len() {
            self.set_status("Invalid workspace index");
            return;
        }

        if workspace_idx == self.config.active_workspace {
            self.set_status("Already on this workspace");
            return;
        }

        if self.pending_workspace_switch.is_some() {
            self.set_status("Workspace switch already in progress");
            return;
        }

        // Save current workspace state
        let _ = self.save_state();

        // Shutdown old WebSocket task
        let old_slack = self.slack.clone();
        tokio::spawn(async move { old_slack.shutdown().await });

        // Update active workspace
        self.config.active_workspace = workspace_idx;
        let _ = self.config.save();

        let workspace_name = self.config.workspaces[workspace_idx].name.clone();
        let workspace_token = self.config.workspaces[workspace_idx].token.clone();
        let workspace_app_token = self.config.workspaces[workspace_idx].app_token.clone();

        // Clear old chats and restore layout synchronously
        self.chats.clear();

        // Load saved layout for this workspace
        let app_state = AppState::load(&self.config).unwrap_or_else(|_| AppState {
            settings: crate::persistence::AppSettings {
                show_reactions: self.show_reactions,
                show_notifications: self.show_notifications,
                compact_mode: self.compact_mode,
                show_emojis: self.show_emojis,
                show_line_numbers: self.show_line_numbers,
                show_timestamps: self.show_timestamps,
                show_chat_list: self.show_chat_list,
                show_user_colors: self.show_user_colors,
                show_borders: self.show_borders,
            },
            aliases: self.aliases.clone(),
            layout: LayoutData::default(),
        });

        // Restore pane tree
        let (pane_tree, required_indices) = if let Some(saved_tree) = app_state.layout.pane_tree {
            let indices = saved_tree.get_pane_indices();
            (saved_tree, indices)
        } else {
            let tree = PaneNode::new_single(0);
            let indices = tree.get_pane_indices();
            (tree, indices)
        };
        self.pane_tree = pane_tree;

        // Restore panes
        let max_required_idx = required_indices.iter().max().copied().unwrap_or(0);
        let total_panes_needed = (max_required_idx + 1)
            .max(app_state.layout.panes.len())
            .max(1);

        self.panes.clear();
        for i in 0..total_panes_needed {
            if let Some(ps) = app_state.layout.panes.get(i) {
                let mut pane = ChatPane::new();
                pane.chat_id = ps.chat_id;
                pane.channel_id_str = ps.channel_id.clone();
                pane.chat_name = ps.chat_name.clone();
                pane.scroll_offset = ps.scroll_offset;
                self.panes.push(pane);
            } else {
                self.panes.push(ChatPane::new());
            }
        }

        if app_state.layout.focused_pane < self.panes.len() {
            self.focused_pane_idx = app_state.layout.focused_pane;
        } else {
            self.focused_pane_idx = 0;
        }

        // Spawn async connection work in background
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = async {
                let slack = SlackClient::new(&workspace_token, &workspace_app_token)
                    .await
                    .map_err(|e| e.to_string())?;
                let my_user_id = slack.get_my_user_id().await.map_err(|e| e.to_string())?;
                slack
                    .start_event_listener(workspace_app_token)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((slack, my_user_id))
            }
            .await;
            let _ = tx.send(result);
        });
        self.pending_workspace_switch = Some(rx);

        self.set_status(&format!("Connecting to workspace: {}...", workspace_name));
    }

    /// Called from the event loop to check if a background workspace switch completed.
    pub fn poll_workspace_switch(&mut self) -> bool {
        let rx = match self.pending_workspace_switch.as_mut() {
            Some(rx) => rx,
            None => return false,
        };

        match rx.try_recv() {
            Ok(Ok((slack, my_user_id))) => {
                self.slack = slack;
                self.my_user_id = my_user_id;
                self.pending_workspace_switch = None;
                self.pending_refresh_chats = true;
                let name = self.config.workspaces[self.config.active_workspace].name.clone();
                self.set_status(&format!("Switched to workspace: {}", name));
                true
            }
            Ok(Err(e)) => {
                self.pending_workspace_switch = None;
                self.set_status(&format!("Workspace switch failed: {}", e));
                false
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.pending_workspace_switch = None;
                self.set_status("Workspace switch failed: task dropped");
                false
            }
        }
    }

    pub fn get_workspace_list(&self) -> Vec<(usize, String, bool)> {
        self.config.workspaces
            .iter()
            .enumerate()
            .map(|(idx, ws)| (idx, ws.name.clone(), idx == self.config.active_workspace))
            .collect()
    }

    pub fn show_workspace_list(&mut self) {
        let workspaces = self.get_workspace_list();
        let mut msg = String::from("Workspaces (Ctrl+1-9 to switch):\n");
        for (idx, name, is_active) in workspaces {
            let marker = if is_active { "* " } else { "  " };
            msg.push_str(&format!("{}{}. {}\n", marker, idx + 1, name));
        }
        self.set_status(&msg);
    }

    pub fn ensure_valid_pane_idx(&mut self) {
        if self.panes.is_empty() {
            self.panes.push(ChatPane::new());
            self.focused_pane_idx = 0;
        } else if self.focused_pane_idx >= self.panes.len() {
            self.focused_pane_idx = self.panes.len() - 1;
        }
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn wrap_spans_hanging(
    spans: &[Span],
    first_width: usize,
    rest_width: usize,
    indent: &str,
) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut remaining = first_width.max(1);
    let rest_width = rest_width.max(1);
    let indent_style = spans.first().map(|span| span.style).unwrap_or_default();
    let mut line_has_content = false;

    let start_new_line = |lines: &mut Vec<Vec<Span<'static>>>,
                          current: &mut Vec<Span<'static>>,
                          remaining: &mut usize,
                          line_has_content: &mut bool| {
        lines.push(std::mem::take(current));
        if !indent.is_empty() {
            current.push(Span::styled(indent.to_string(), indent_style));
        }
        *remaining = rest_width;
        *line_has_content = false;
    };

    for span in spans {
        let style = span.style;
        let mut text = span.content.as_ref();
        while !text.is_empty() {
            let (segment, next) = if let Some(pos) = text.find('\n') {
                (&text[..pos], Some(&text[pos + 1..]))
            } else {
                (text, None)
            };

            if !segment.is_empty() {
                let mut tokens: Vec<(String, bool)> = Vec::new();
                let mut buf = String::new();
                let mut buf_is_space: Option<bool> = None;
                for ch in segment.chars() {
                    let is_space = ch.is_whitespace();
                    if let Some(current_space) = buf_is_space {
                        if current_space == is_space {
                            buf.push(ch);
                        } else {
                            tokens.push((std::mem::take(&mut buf), current_space));
                            buf.push(ch);
                            buf_is_space = Some(is_space);
                        }
                    } else {
                        buf.push(ch);
                        buf_is_space = Some(is_space);
                    }
                }
                if let Some(current_space) = buf_is_space {
                    if !buf.is_empty() {
                        tokens.push((buf, current_space));
                    }
                }

                for (token, is_space) in tokens {
                    let token_width = UnicodeWidthStr::width(token.as_str());
                    if is_space {
                        if line_has_content && token_width <= remaining {
                            current.push(Span::styled(token, style));
                            remaining = remaining.saturating_sub(token_width);
                        }
                        continue;
                    }

                    if token_width <= remaining {
                        current.push(Span::styled(token, style));
                        remaining = remaining.saturating_sub(token_width);
                        line_has_content = true;
                        continue;
                    }

                    if line_has_content {
                        start_new_line(&mut lines, &mut current, &mut remaining, &mut line_has_content);
                    }

                    if token_width <= remaining {
                        current.push(Span::styled(token, style));
                        remaining = remaining.saturating_sub(token_width);
                        line_has_content = true;
                        continue;
                    }

                    let mut word_buf = String::new();
                    for ch in token.chars() {
                        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
                        if line_has_content && width > remaining {
                            if !word_buf.is_empty() {
                                current.push(Span::styled(std::mem::take(&mut word_buf), style));
                            }
                            start_new_line(&mut lines, &mut current, &mut remaining, &mut line_has_content);
                        }
                        if remaining == 0 && line_has_content {
                            if !word_buf.is_empty() {
                                current.push(Span::styled(std::mem::take(&mut word_buf), style));
                            }
                            start_new_line(&mut lines, &mut current, &mut remaining, &mut line_has_content);
                        }

                        word_buf.push(ch);
                        remaining = remaining.saturating_sub(width);
                        line_has_content = true;
                    }
                    if !word_buf.is_empty() {
                        current.push(Span::styled(word_buf, style));
                    }
                }
            }

            if next.is_some() {
                start_new_line(&mut lines, &mut current, &mut remaining, &mut line_has_content);
            }
            if let Some(next_text) = next {
                text = next_text;
            } else {
                break;
            }
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    s[..idx].char_indices().last().map(|(i, _)| i).unwrap_or(0)
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut iter = s[idx..].char_indices();
    iter.next();
    if let Some((next_i, _)) = iter.next() {
        idx + next_i
    } else {
        s.len()
    }
}

fn line_bounds(s: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(s.len());
    let line_start = s[..cursor]
        .rfind('\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_end = s[cursor..]
        .find('\n')
        .map(|idx| cursor + idx)
        .unwrap_or_else(|| s.len());
    (line_start, line_end)
}

fn column_in_line(s: &str, line_start: usize, cursor: usize) -> usize {
    s[line_start..cursor.min(s.len())].chars().count()
}

fn index_from_column(s: &str, line_start: usize, line_end: usize, target_col: usize) -> usize {
    let mut col = 0;
    for (byte_idx, _) in s[line_start..line_end].char_indices() {
        if col >= target_col {
            return line_start + byte_idx;
        }
        col += 1;
    }
    line_end
}

fn cursor_visual_pos(s: &str, cursor: usize, width: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    let mut line = 0;
    let mut col = 0;
    for (byte_idx, ch) in s.char_indices() {
        if byte_idx >= cursor {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
            continue;
        }
        col += 1;
        if col >= width {
            line += 1;
            col = 0;
        }
    }
    (line, col)
}
