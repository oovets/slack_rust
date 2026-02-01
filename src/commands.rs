use anyhow::Result;

use crate::app::App;
use crate::widgets::FilterType;

pub struct Command {
    pub name: String,
    pub args: Vec<String>,
}

impl Command {
    pub fn parse(text: &str) -> Option<Self> {
        if !text.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let name = parts[0][1..].to_string();
        let args = parts[1..].iter().map(|s| s.to_string()).collect();

        Some(Command { name, args })
    }
}

pub struct CommandHandler;

impl CommandHandler {
    pub fn new() -> Self {
        Self
    }

    pub async fn handle_command(&mut self, app: &mut App, text: &str) -> Result<()> {
        let cmd = match Command::parse(text) {
            Some(c) => c,
            None => return Ok(()),
        };

        match cmd.name.as_str() {
            "thread" | "t" => {
                Self::handle_thread(app, &cmd).await?;
            }
            "react" => {
                Self::handle_react(app, &cmd).await?;
            }
            "filter" => {
                Self::handle_filter(app, &cmd).await?;
            }
            "alias" => {
                Self::handle_alias(app, &cmd).await?;
            }
            "unalias" => {
                Self::handle_unalias(app, &cmd).await?;
            }
            "workspace" | "ws" => {
                Self::handle_workspace(app, &cmd).await?;
            }
            "leave" => {
                Self::handle_leave(app).await?;
            }
            "help" | "h" => {
                Self::handle_help(app).await?;
            }
            "media" => {
                Self::handle_media(app, &cmd).await?;
            }
            // /1, /2, /3... for quick workspace switching
            name if name.chars().all(|c| c.is_ascii_digit()) => {
                if let Ok(num) = name.parse::<usize>() {
                    if num > 0 && num <= app.config.workspaces.len() {
                        app.switch_workspace(num - 1);
                    } else {
                        app.set_status(&format!("Invalid workspace number: {}", num));
                    }
                } else {
                    app.set_status(&format!("Unknown command: /{}", cmd.name));
                }
            }
            _ => {
                app.set_status(&format!("Unknown command: /{}", cmd.name));
            }
        }

        Ok(())
    }

    async fn handle_thread(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.is_empty() {
            app.set_status("Usage: /thread N or /t N");
            return Ok(());
        }

        let num_str = cmd.args[0].trim_start_matches('#');
        let num: usize = match num_str.parse() {
            Ok(n) => n,
            Err(_) => {
                app.set_status("Usage: /thread N (where N is the message number)");
                return Ok(());
            }
        };

        let pane = &app.panes[app.focused_pane_idx];
        if pane.msg_data.is_empty() {
            app.set_status("No messages loaded");
            return Ok(());
        }

        if num < 1 || num > pane.msg_data.len() {
            app.set_status(&format!(
                "Message #{} not found (1-{})",
                num,
                pane.msg_data.len()
            ));
            return Ok(());
        }

        let msg = &pane.msg_data[num - 1];
        let thread_ts = msg.ts.clone();
        let parent_user = msg.sender_name.clone();
        let channel_id_str = match &pane.channel_id_str {
            Some(id) => id.clone(),
            None => {
                app.set_status("No channel selected");
                return Ok(());
            }
        };

        app.open_thread(&channel_id_str, &thread_ts, &parent_user)
            .await?;
        Ok(())
    }

    async fn handle_react(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.is_empty() {
            app.set_status("Usage: /react <emoji> [message_number]");
            return Ok(());
        }

        let pane = &app.panes[app.focused_pane_idx];
        if let Some(channel_id) = &pane.channel_id_str {
            let emoji = &cmd.args[0];
            let msg_idx = if cmd.args.len() > 1 {
                cmd.args[1]
                    .parse::<usize>()
                    .unwrap_or(pane.msg_data.len().saturating_sub(1))
            } else {
                pane.msg_data.len().saturating_sub(1)
            };

            if let Some(msg) = pane.msg_data.get(msg_idx) {
                let timestamp = &msg.ts;
                match app.slack.add_reaction(channel_id, timestamp, emoji).await {
                    Ok(_) => app.set_status(&format!("Added reaction :{emoji}:")),
                    Err(e) => app.set_status(&format!("Failed to add reaction: {}", e)),
                }
            }
        }

        Ok(())
    }

    async fn handle_filter(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.is_empty() {
            let pane = &mut app.panes[app.focused_pane_idx];
            pane.filter_type = None;
            pane.filter_value = None;
            pane.invalidate_cache();
            app.set_status("Filter cleared");
            return Ok(());
        }

        let filter_str = &cmd.args[0].to_lowercase();
        let filter_type = match filter_str.as_str() {
            "sender" => FilterType::Sender,
            "media" => FilterType::Media,
            "link" => FilterType::Link,
            _ => {
                app.set_status("Usage: /filter [sender|media|link] [value]");
                return Ok(());
            }
        };

        let filter_value = if cmd.args.len() > 1 {
            Some(cmd.args[1..].join(" "))
        } else {
            None
        };

        let pane = &mut app.panes[app.focused_pane_idx];
        pane.filter_type = Some(filter_type);
        pane.filter_value = filter_value.clone();
        pane.invalidate_cache();

        let msg = if let Some(val) = filter_value {
            format!("Filter: {:?} = {}", &filter_type, val)
        } else {
            format!("Filter: {:?}", filter_type)
        };
        app.set_status(&msg);

        Ok(())
    }

    async fn handle_alias(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.len() < 2 {
            app.set_status("Usage: /alias <name> <value>");
            return Ok(());
        }

        let alias_name = &cmd.args[0];
        let alias_value = cmd.args[1..].join(" ");

        app.aliases.insert(alias_name.clone(), alias_value.clone());
        app.set_status(&format!("Alias '{}' = '{}'", alias_name, alias_value));

        Ok(())
    }

    async fn handle_unalias(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.is_empty() {
            app.set_status("Usage: /unalias <name>");
            return Ok(());
        }

        let alias_name = &cmd.args[0];
        if app.aliases.remove(alias_name).is_some() {
            app.set_status(&format!("Removed alias '{}'", alias_name));
        } else {
            app.set_status(&format!("Alias '{}' not found", alias_name));
        }

        Ok(())
    }

    async fn handle_leave(app: &mut App) -> Result<()> {
        let pane = &app.panes[app.focused_pane_idx];
        let channel_id = match &pane.channel_id_str {
            Some(id) => id.clone(),
            None => {
                app.set_status("No channel selected");
                return Ok(());
            }
        };

        match app.slack.leave_conversation(&channel_id).await {
            Ok(_) => {
                // Remove from chat list
                app.chats.retain(|c| c.id != channel_id);
                if app.selected_chat_idx >= app.chats.len() {
                    app.selected_chat_idx = app.chats.len().saturating_sub(1);
                }
                app.set_status("Left channel");
            }
            Err(e) => {
                app.set_status(&format!("Failed to leave: {}", e));
            }
        }

        Ok(())
    }

    async fn handle_workspace(app: &mut App, cmd: &Command) -> Result<()> {
        if cmd.args.is_empty() {
            // Show workspace list
            app.show_workspace_list();
            return Ok(());
        }

        // Switch to workspace by number or name
        let arg = &cmd.args[0];
        if let Ok(idx) = arg.parse::<usize>() {
            // Switch by number (1-indexed)
            if idx > 0 && idx <= app.config.workspaces.len() {
                app.switch_workspace(idx - 1);
            } else {
                app.set_status(&format!("Invalid workspace number: {}", idx));
            }
        } else {
            // Switch by name
            let workspace_idx = app.config.workspaces
                .iter()
                .position(|ws| ws.name.eq_ignore_ascii_case(arg));
            
            if let Some(idx) = workspace_idx {
                app.switch_workspace(idx);
            } else {
                app.set_status(&format!("Workspace '{}' not found", arg));
            }
        }

        Ok(())
    }

    async fn handle_media(app: &mut App, cmd: &Command) -> Result<()> {
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
        
        log_to_file("=== HANDLE MEDIA COMMAND DEBUG ===");
        log_to_file(&format!("Command args: {:?}", cmd.args));
        
        if cmd.args.is_empty() {
            app.set_status("Usage: /media #N (download and open media from message N)");
            return Ok(());
        }

        let num_str = cmd.args[0].trim_start_matches('#');
        log_to_file(&format!("Parsing message number from: {}", num_str));
        
        let msg_num: usize = match num_str.parse() {
            Ok(n) => {
                log_to_file(&format!("Parsed message number: {}", n));
                n
            }
            Err(e) => {
                log_to_file(&format!("Failed to parse message number: {}", e));
                app.set_status("Invalid message number");
                return Ok(());
            }
        };

        // Get the focused pane
        let pane = &app.panes[app.focused_pane_idx];
        log_to_file(&format!("Focused pane has {} messages", pane.msg_data.len()));
        log_to_file(&format!("Channel ID: {:?}", pane.channel_id_str));
        
        if msg_num == 0 || msg_num > pane.msg_data.len() {
            log_to_file(&format!("Message #{} not found (valid range: 1-{})", msg_num, pane.msg_data.len()));
            app.set_status(&format!("Message #{} not found", msg_num));
            return Ok(());
        }

        let msg = &pane.msg_data[msg_num - 1];
        log_to_file(&format!("Message #{}: media_type={:?}, file_urls={:?}, file_names={:?}", 
            msg_num, msg.media_type, msg.file_urls, msg.file_names));
        log_to_file(&format!("Message text: {}", msg.text));
        
        if msg.file_ids.is_empty() {
            log_to_file(&format!("Message #{} has no file_ids", msg_num));
            app.set_status(&format!("Message #{} has no media", msg_num));
            return Ok(());
        }

        let file_id = &msg.file_ids[0];
        let file_name = msg.file_names.get(0).cloned().unwrap_or_else(|| "file".to_string());
        
        log_to_file(&format!("Downloading file_id: {}, file_name: {}", file_id, file_name));
        
        // Try to get a shareable public URL using files.sharedPublicURL API
        // This gives us a direct download URL that works without HTML redirects
        match app.slack.get_shared_public_url(file_id, &file_name).await {
            Ok(file_path) => {
                log_to_file(&format!("File downloaded successfully to: {:?}", file_path));
                // Open file with system default application
                #[cfg(target_os = "macos")]
                {
                    use std::process::Command;
                    log_to_file("Opening file with 'open' command");
                    let output = Command::new("open").arg(&file_path).output();
                    log_to_file(&format!("Open command result: {:?}", output));
                }
                #[cfg(target_os = "linux")]
                {
                    use std::process::Command;
                    log_to_file("Opening file with 'xdg-open' command");
                    let output = Command::new("xdg-open").arg(&file_path).output();
                    log_to_file(&format!("Xdg-open command result: {:?}", output));
                }
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                {
                    app.set_status(&format!("Downloaded to: {}", file_path.display()));
                }
                app.set_status(&format!("Opened media from message #{}", msg_num));
            }
            Err(e) => {
                log_to_file(&format!("Failed to get shared public URL: {}. Trying fallback...", e));
                // Fallback: try direct download from file_urls if available
                if !msg.file_urls.is_empty() {
                    let file_url = &msg.file_urls[0];
                    log_to_file(&format!("Trying direct download from URL: {}", file_url));
                    match app.slack.download_file_from_url(file_url, &file_name).await {
                        Ok(file_path) => {
                            log_to_file(&format!("File downloaded successfully via fallback to: {:?}", file_path));
                            #[cfg(target_os = "macos")]
                            {
                                use std::process::Command;
                                let _ = Command::new("open").arg(&file_path).output();
                            }
                            #[cfg(target_os = "linux")]
                            {
                                use std::process::Command;
                                let _ = Command::new("xdg-open").arg(&file_path).output();
                            }
                            app.set_status(&format!("Opened media from message #{}", msg_num));
                        }
                        Err(fallback_err) => {
                            log_to_file(&format!("Fallback also failed: {}", fallback_err));
                            app.set_status(&format!("Failed to download media: {}", fallback_err));
                        }
                    }
                } else {
                    log_to_file(&format!("No file_urls available for fallback"));
                    app.set_status(&format!("Failed to download media: {}", e));
                }
            }
        }

        Ok(())
    }

    async fn handle_help(app: &mut App) -> Result<()> {
        app.set_status("Commands: /thread N | /react <emoji> | /filter | /workspace | /leave | /alias | /media #N | /help");
        Ok(())
    }
}
