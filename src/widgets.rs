use ratatui::text::Line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    Sender,
    Media,
    Link,
}

/// Represents a single message with all its metadata for display
#[derive(Clone, Debug)]
pub struct MessageData {
    pub sender_name: String,
    pub text: String,
    pub is_outgoing: bool,
    pub ts: String,                    // Slack timestamp string (for thread_ts)
    pub reactions: Vec<(String, u32)>, // (emoji_name, count)
    pub reply_count: u32,
    pub forwarded_text: Option<String>,
    pub mentions_me: bool, // True if this message mentions the current user
    pub local_echo_id: Option<u64>, // Unique ID for local echo deduplication
}

pub struct ChatPane {
    pub chat_id: Option<i64>, // Stored as i64 for compatibility, parsed from String
    pub channel_id_str: Option<String>, // String channel ID for API calls
    pub chat_name: String,
    pub username: Option<String>,
    pub messages: Vec<String>,      // Formatted display lines
    pub msg_data: Vec<MessageData>, // Raw message data for formatting
    pub scroll_offset: usize,
    pub reply_to_message: Option<i32>, // Message ID to reply to
    pub reply_preview: Option<String>, // Text shown in reply preview bar
    pub thread_ts: Option<String>,     // If set, this pane shows a thread
    pub filter_type: Option<FilterType>,
    pub filter_value: Option<String>,
    pub typing_indicator: Option<String>, // "Name is typing..."
    pub typing_expire: Option<std::time::Instant>,
    pub online_status: String,
    pub pinned_message: Option<String>,
    pub dirty: bool,
    pub cached_lines: Option<Vec<Line<'static>>>,
    pub cached_line_count: Option<(u16, usize)>, // (width, wrapped_count)
    pub input_buffer: String,                    // Per-pane input buffer
    pub input_cursor: usize,                     // Byte index cursor into input_buffer
    pub tab_complete_state: Option<TabCompleteState>,
}

#[derive(Clone, Debug)]
pub struct TabCompleteState {
    pub before: String,          // Text before @prefix
    pub after: String,           // Text after cursor when tab completion started
    pub candidates: Vec<String>, // Matching names
    pub index: usize,            // Current candidate index
}

impl ChatPane {
    pub fn new() -> Self {
        Self {
            chat_id: None,
            channel_id_str: None,
            chat_name: String::from("No chat selected"),
            username: None,
            messages: Vec::new(),
            msg_data: Vec::new(),
            scroll_offset: 0,
            reply_to_message: None,
            reply_preview: None,
            thread_ts: None,
            filter_type: None,
            filter_value: None,
            typing_indicator: None,
            typing_expire: None,
            online_status: String::new(),
            pinned_message: None,
            input_buffer: String::new(),
            input_cursor: 0,
            dirty: true,
            cached_lines: None,
            cached_line_count: None,
            tab_complete_state: None,
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.msg_data.clear();
        self.scroll_offset = 0;
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.invalidate_cache();
    }

    pub fn invalidate_cache(&mut self) {
        self.dirty = true;
        self.cached_lines = None;
        self.cached_line_count = None;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn show_typing_indicator(&mut self, name: &str) {
        self.typing_indicator = Some(format!("{} is typing...", name));
        self.typing_expire = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
    }

    pub fn hide_typing_indicator(&mut self) {
        self.typing_indicator = None;
        self.typing_expire = None;
    }

    pub fn check_typing_expired(&mut self) {
        if let Some(expire) = self.typing_expire {
            if std::time::Instant::now() >= expire {
                self.hide_typing_indicator();
            }
        }
    }

    pub fn hide_reply_preview(&mut self) {
        self.reply_preview = None;
    }

    /// Build the header text including online status, username, pinned message, typing indicator
    pub fn header_text(&self) -> String {
        let mut header = self.chat_name.clone();

        if !self.online_status.is_empty() {
            header.push_str(&format!(" [{}]", self.online_status));
        }

        if let Some(ref username) = self.username {
            if !username.is_empty() {
                header.push_str(&format!(" {}", username));
            }
        }

        if let Some(ref pinned) = self.pinned_message {
            header.push_str(&format!(" | Pinned: {}", pinned));
        }

        if let Some(ref typing) = self.typing_indicator {
            header.push_str(&format!(" {}", typing));
        }

        header
    }
}

impl Default for ChatPane {
    fn default() -> Self {
        Self::new()
    }
}
