# Slack Client (Rust TUI)

Terminal-native Slack client built with Rust and ratatui. Supports multiple panes, live updates via Socket Mode, and a fast keyboard-first workflow.

## Features
- Split view: vertical/horizontal splits, per-pane focus, mouse click to focus/open.
- Live updates: messages appear across all open panes; typing indicators, desktop notifications.
- Channel list: top “New” section for unread chats, per-section grouping (public/private/DM/group/bots), unread badge + red highlight.
- Message view: reactions and emoji rendering (toggleable), thread open in its own pane, optional timestamps/line numbers/compact mode.
- Commands: `/react`, `/filter`, `/alias`, `/unalias`, `/thread`, `/leave`, `/help`.
- Persistence: saves layout, settings, aliases, and open panes between sessions.

## Prerequisites
- Rust 1.70+ (`rustup` recommended).
- Slack app with Socket Mode enabled.
  - App-Level Token with `connections:write` (xapp-...).
  - User OAuth Token (xoxp-...) or Bot User OAuth Token (xoxb-...). The client accepts either; user tokens typically have wider access.
  - Recommended scopes: `channels:history`, `channels:read`, `chat:write`, `groups:history`, `groups:read`, `im:history`, `im:read`, `mpim:history`, `mpim:read`, `reactions:write`, `users:read`.

## Quick Start
```bash
git clone <repo> slack_rust
cd slack_rust
cargo build --release   # or ./build.sh
./target/release/slack_client_rs
```
On first run, enter your Bot Token (xoxb-), App Token (xapp-), and workspace name. Config is stored in `~/.config/slack_client_rs/`.

## Key Controls
- Focus/navigation: `Tab` (switch list/panes), `↑/↓`, `PageUp/PageDown`, `Home/End`.
- Send/open: `Enter` (open chat in list or send in pane), `Esc` (cancel reply).
- Pane layout: `Ctrl+V` split vertical, `Ctrl+B` split horizontal, `Ctrl+K` toggle split direction, `Ctrl+W` close pane, `Ctrl+L` clear pane, `Ctrl+S` toggle channel list.
- Display toggles: `Ctrl+E` reactions, `Ctrl+O` emojis, `Ctrl+T` timestamps, `Ctrl+G` line numbers, `Ctrl+D` compact mode.
- Refresh/quit: `Ctrl+R` refresh chats, `Ctrl+Q` quit (state auto-saved).

## Commands
- `/react <emoji> [msg#]` – add reaction to a message (default last).
- `/filter [sender|media|link] [value]` – apply filter; `/filter` clears.
- `/alias <name> <value>` / `/unalias <name>`.
- `/thread <msg#>` – open replies in a new pane.
- `/leave` – leave current channel.
- `/help` – show help.

## Project Layout
`src/main.rs` (entry + event loop), `app.rs` (UI + panes), `slack.rs` (HTTP + Socket Mode), `widgets.rs` (chat data), `split_view.rs` (layout tree), `commands.rs`, `formatting.rs`, `persistence.rs`, `config.rs`, `utils.rs`.
