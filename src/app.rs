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
    pub async fn new() -> Result<Self> {
        let config = Config::load()?;
        let slack = SlackClient::new(&config).await?;
        let my_user_id = slack.get_my_user_id().await?;

        // Start event listener
        slack.start_event_listener(config.app_token.clone()).await?;

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
            // Load messages for this channel
            match self.slack.get_conversation_history(&channel_id, 50).await {
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
                        let msg_data = crate::widgets::MessageData {
                            sender_name,
                            text: slack_msg.text.clone(),
                            is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                            ts: slack_msg.ts.clone(),
                            reactions,
                            reply_count: slack_msg.reply_count.unwrap_or(0),
                            forwarded_text: forwarded_preview(&slack_msg.attachments),
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
                                                };
                                                pane.msg_data.push(msg_data);
                                                pane.format_cache.clear();
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
                                            };
                                            pane.msg_data.push(msg_data);
                                            pane.format_cache.clear();
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

                    // Send notification
                    if self.show_notifications && !is_bot && !is_self {
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
                            &format!("Slack: {}", title),
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
        pane.format_cache.clear();

        // Clear unread counter when opening the chat
        if let Some(chat_info) = self.chats.get_mut(self.selected_chat_idx) {
            chat_info.unread = 0;
        }

        // Load messages
        match self.slack.get_conversation_history(&chat.id, 50).await {
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
                    let msg_data = crate::widgets::MessageData {
                        sender_name,
                        text: slack_msg.text.clone(),
                        is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                        ts: slack_msg.ts.clone(),
                        reactions,
                        reply_count: slack_msg.reply_count.unwrap_or(0),
                        forwarded_text: forwarded_preview(&slack_msg.attachments),
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
            .get_thread_replies(channel_id_str, thread_ts, 50)
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
                    let msg_data = crate::widgets::MessageData {
                        sender_name,
                        text: slack_msg.text.clone(),
                        is_outgoing: slack_msg.user.as_deref() == Some(&self.my_user_id),
                        ts: slack_msg.ts.clone(),
                        reactions,
                        reply_count: 0,
                        forwarded_text: forwarded_preview(&slack_msg.attachments),
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
        let pane_idx = self.focused_pane_idx;
        let input = self.panes[pane_idx].input_buffer.trim().to_string();

        if input.is_empty() {
            return Ok(());
        }

        // Check if it's a command
        if input.starts_with('/') {
            let mut handler = CommandHandler::new();
            handler.handle_command(self, &input).await?;
            self.panes[pane_idx].input_buffer.clear();
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
                }
                Err(e) => {
                    self.set_status(&format!("Failed to send: {}", e));
                }
            }
        }

        Ok(())
    }

    pub fn draw(&mut self, f: &mut Frame) {
        // Check typing indicators for expiry
        for pane in &mut self.panes {
            pane.check_typing_expired();
        }

        // Check status message expiry
        if let Some(expire) = self.status_expire {
            if std::time::Instant::now() >= expire {
                self.status_message = None;
                self.status_expire = None;
            }
        }

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
        let header_height = if self.compact_mode { 2 } else { 3 };
        let input_height = if self.compact_mode { 2 } else { 3 };
        let reply_height = if self.compact_mode { 2 } else { 3 };
        let constraints = if has_reply_preview {
            vec![
                Constraint::Length(header_height),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(reply_height),
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

        // Estimate total wrapped lines for scroll calculation
        let msg_area_height = chunks[1].height.saturating_sub(2) as usize; // borders
        let msg_area_width = chunks[1].width.saturating_sub(2) as usize; // borders
        let total_wrapped_lines: usize = if msg_area_width > 0 {
            pane.msg_data
                .iter()
                .enumerate()
                .map(|(idx, msg)| {
                    // Rough estimate: prefix + text length / width, at least 1 line per message
                    let mut line_len = msg.sender_name.len() + 2 + msg.text.len(); // name: text
                    if self.show_line_numbers {
                        line_len += format!("#{} ", idx + 1).len();
                    }
                    if self.show_timestamps {
                        line_len += 20; // approx timestamp width
                    }
                    let mut lines = (line_len / msg_area_width) + 1;
                    
                    // Add lines for quoted/forwarded message (max 3 lines)
                    if let Some(ref fwd) = msg.forwarded_text {
                        let fwd_lines = fwd.lines().count().min(3);
                        lines += fwd_lines;
                    }
                    
                    lines
                })
                .sum()
        } else {
            pane.msg_data.len()
        };
        let max_scroll = total_wrapped_lines.saturating_sub(msg_area_height);
        let scroll_offset = pane.scroll_offset.min(max_scroll);

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

            let mut spans = Vec::new();

            if show_line_numbers {
                spans.push(Span::styled(
                    format!("#{} ", idx + 1),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            if let Some(ts_fmt) = format_ts(&msg.ts) {
                spans.push(Span::styled(
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
            spans.push(Span::styled(format!("{}: ", msg.sender_name), username_style));
            spans.push(Span::raw(formatted_text));

            // Thread reply indicator
            if msg.reply_count > 0 {
                spans.push(Span::styled(
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
                spans.push(Span::styled(
                    format!("  {}", reaction_str),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            message_lines.push(Line::from(spans));

            // Show quoted/forwarded message as indented block (max 3 lines)
            if let Some(ref fwd) = msg.forwarded_text {
                // Split forwarded text into lines and show as quote (limit to 3 lines)
                let mut line_count = 0;
                for line in fwd.lines() {
                    if line_count >= 3 {
                        message_lines.push(Line::from(Span::styled(
                            "│ ...",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::ITALIC),
                        )));
                        break;
                    }
                    message_lines.push(Line::from(Span::styled(
                        format!("│ {}", line),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                    line_count += 1;
                }
            }
        }

        let messages_block = if self.show_borders {
            Block::default().borders(Borders::ALL).title("Messages")
        } else {
            Block::default().padding(Padding::left(2))
        };
        
        let messages = Paragraph::new(message_lines)
            .block(messages_block)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset as u16, 0));

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

        let input_block = if self.show_borders {
            Block::default().borders(Borders::ALL).title("Input")
        } else {
            Block::default()
        };
        let input = Paragraph::new(pane.input_buffer.as_str())
            .block(input_block)
            .style(input_style)
            .wrap(Wrap { trim: false }); // Enable text wrapping in input

        f.render_widget(input, input_chunk);
        
        // Set cursor position in the active input box
        if is_focused && !self.focus_on_chat_list {
            // Calculate cursor position with wrapping support
            let border_offset = if self.show_borders { 2 } else { 0 };
            let input_width = input_chunk.width.saturating_sub(border_offset) as usize;
            let text_len = pane.input_buffer.len();
            
            if input_width > 0 {
                let cursor_line = text_len / input_width;
                let cursor_col = text_len % input_width;
                
                let border_padding = if self.show_borders { 1 } else { 0 };
                let cursor_x = input_chunk.x + border_padding + cursor_col as u16;
                let cursor_y = input_chunk.y + border_padding + cursor_line as u16;
                f.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }

    pub fn set_status(&mut self, message: &str) {
        self.status_message = Some(message.to_string());
        self.status_expire = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
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
        self.panes[self.focused_pane_idx].scroll_up();
    }

    pub fn scroll_down(&mut self) {
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
        self.panes[self.focused_pane_idx].input_buffer.push(c);
        // Reset tab completion on any new input
        self.panes[self.focused_pane_idx].tab_complete_state = None;
    }

    pub fn backspace(&mut self) {
        self.panes[self.focused_pane_idx].input_buffer.pop();
        self.panes[self.focused_pane_idx].tab_complete_state = None;
    }

    pub fn tab_complete(&mut self) {
        use crate::widgets::TabCompleteState;

        let pane = &mut self.panes[self.focused_pane_idx];

        if let Some(ref mut state) = pane.tab_complete_state {
            // Cycle to next candidate
            if state.candidates.is_empty() {
                return;
            }
            state.index = (state.index + 1) % state.candidates.len();
            let replacement = &state.candidates[state.index];
            pane.input_buffer.truncate(state.start_pos);
            pane.input_buffer.push_str(&format!("@{} ", replacement));
        } else {
            // Find @prefix at cursor (end of input)
            let input = &pane.input_buffer;
            let at_pos = input.rfind('@');
            if at_pos.is_none() {
                return;
            }
            let at_pos = at_pos.unwrap();
            let prefix = &input[at_pos + 1..];
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
            let start_pos = at_pos;
            pane.input_buffer.truncate(start_pos);
            pane.input_buffer.push_str(&format!("@{} ", replacement));

            pane.tab_complete_state = Some(TabCompleteState {
                start_pos,
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
    }

    pub fn toggle_reactions(&mut self) {
        self.show_reactions = !self.show_reactions;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_emojis(&mut self) {
        self.show_emojis = !self.show_emojis;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_timestamps(&mut self) {
        self.show_timestamps = !self.show_timestamps;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_compact_mode(&mut self) {
        self.compact_mode = !self.compact_mode;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_user_colors(&mut self) {
        self.show_user_colors = !self.show_user_colors;
        for pane in &mut self.panes {
            pane.format_cache.clear();
        }
    }

    pub fn toggle_borders(&mut self) {
        self.show_borders = !self.show_borders;
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
}
