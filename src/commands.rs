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
            pane.format_cache.clear();
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
        pane.format_cache.clear();

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

    async fn handle_help(app: &mut App) -> Result<()> {
        app.set_status("Commands: /thread N | /react <emoji> | /filter | /workspace | /leave | /alias | /help");
        Ok(())
    }
}
