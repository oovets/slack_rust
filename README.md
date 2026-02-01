# Slack Client (Rust TUI)

Terminal-native Slack client built with Rust and ratatui. Supports multiple panes, live updates via Socket Mode, and a fast keyboard-first workflow.

## Overview

This is a fully-featured terminal-based Slack client that brings the power of Slack to your terminal. Built for efficiency, it offers a keyboard-first interface with mouse support, multi-pane workflows, and real-time synchronization with your Slack workspace.

## Key Features

### Multi-Workspace Support
- **Multiple Workspaces**: Configure and switch between multiple Slack workspaces
- **Quick Switching**: Use `Ctrl+1` through `Ctrl+9` to instantly switch workspaces
- **Workspace List**: View all configured workspaces with `Ctrl+N` or `/workspace`
- **Per-Workspace State**: Each workspace maintains its own chat list and state
- **Seamless Migration**: Automatically converts old single-workspace configs

### Split View & Multi-Pane Workspace
- **Flexible Layouts**: Split your workspace vertically (`Ctrl+V`) or horizontally (`Ctrl+B`)
- **Multiple Chats**: Keep multiple conversations open simultaneously in different panes
- **Per-Pane Focus**: Each pane maintains its own state, scroll position, and input buffer
- **Mouse Support**: Click to focus panes or open channels from the list
- **Dynamic Resizing**: Toggle split direction (`Ctrl+K`), close panes (`Ctrl+W`), or clear pane content (`Ctrl+L`)
- **Collapsible Sidebar**: Hide/show the channel list (`Ctrl+S`) for more screen space

### Real-Time Communication
- **Live Updates**: Messages appear instantly across all open panes via Socket Mode
- **Typing Indicators**: See when other users are typing in the current channel
- **Desktop Notifications**: Get notified of new messages even while working in other terminals
- **Auto-Refresh**: New messages are automatically fetched and displayed
- **Thread Support**: Open message threads in dedicated panes with `/thread <msg#>` or `/t <msg#>`

### Smart Channel List
- **"New" Section**: Channels with unread messages appear at the top for quick access
- **Organized Sections**: 
  - Public Channels
  - Private Channels
  - Group Chats
  - Direct Messages
  - Bots & Apps
- **Visual Indicators**: Unread badges and red highlighting for channels with new messages
- **Quick Navigation**: Use arrow keys to browse, `Enter` to open

### Customizable Message Display
- **Emoji Rendering**: Full emoji support with Unicode rendering (toggle with `Ctrl+O`)
- **Reactions**: Display and add emoji reactions (toggle with `Ctrl+E`, add with `/react`)
- **Timestamps**: Optional message timestamps (toggle with `Ctrl+T`)
- **Line Numbers**: Number each message for easy reference (toggle with `Ctrl+G`)
- **Compact Mode**: Reduce spacing for more messages on screen (toggle with `Ctrl+D`)
- **Color-Coded Usernames**: Each user gets a unique, consistent color for better visual distinction (toggle with `Ctrl+U`)
- **Borderless Mode**: Remove all borders for a cleaner, minimalist interface (toggle with `Ctrl+Y`)
- **Formatting Cache**: Smart caching for smooth scrolling in long conversations

### Powerful Commands
- `/react <emoji> [msg#]` – Add emoji reactions to messages
- `/filter [sender|media|link] [value]` – Filter messages by sender, media attachments, or links
- `/alias <name> <value>` – Create command shortcuts or text expansions
- `/unalias <name>` – Remove an alias
- `/thread <msg#>` or `/t <msg#>` – Open a message thread in a new pane
- `/leave` – Leave the current channel
- `/help` or `/h` – Show help information

### Session Persistence
- **Layout Saving**: Your pane layout and split configuration are saved between sessions
- **Open Chats**: All open channels are restored when you restart
- **Settings**: Display preferences (timestamps, emojis, etc.) persist
- **Aliases**: Custom aliases are saved in `~/.config/slack_client_rs/aliases.json`
- **Scroll Positions**: Each pane remembers where you were in the conversation

### Advanced Features
- **Message Filtering**: Filter by sender, media content, or links to find what you need
- **Tab Completion**: Press `Tab` to auto-complete user mentions when typing `@`
- **Multi-line Input**: Compose longer messages with `Shift+Enter` and edit with cursor keys
- **Reply Context**: Reply to specific messages with visual context
- **Forwarded Messages**: View forwarded content and attachments
- **User Cache**: Fast display with cached user names and info

## Prerequisites
- **Rust 1.70+** (`rustup` recommended for easy installation)
- **Slack App** with Socket Mode enabled:
  - **App-Level Token** with `connections:write` scope (starts with `xapp-...`)
  - **User OAuth Token** (starts with `xoxp-...`) or **Bot User OAuth Token** (starts with `xoxb-...`)
    - The client accepts either token type
    - User tokens typically have wider access to channels and DMs

### Setting Up Your Slack App
1. Go to [api.slack.com/apps](https://api.slack.com/apps)
2. Create a new app or select an existing one
3. Enable **Socket Mode** under Settings → Socket Mode
4. Generate an **App-Level Token** with `connections:write` scope
5. Add **OAuth Scopes** under OAuth & Permissions
    - **Recommended OAuth Scopes**:
    - `channels:history` – Read messages in public channels
    - `channels:read` – View public channels
    - `chat:write` – Send messages
    - `groups:history` – Read messages in private channels
    - `groups:read` – View private channels
    - `im:history` – Read direct messages
    - `im:read` – View direct messages
    - `mpim:history` – Read group direct messages
    - `mpim:read` – View group direct messages
    - `reactions:write` – Add emoji reactions
    - `users:read` – Get user information
6. Enable **Event Subscriptions** under Features → Event Subscriptions
    - Toggle **Enable Events** to ON
    - Under **Subscribe to bot events** (or **Subscribe to events on behalf of users** for User Tokens), add:
      - `message.channels` – Receive messages in public channels (includes edits and deletions)
      - `message.groups` – Receive messages in private channels (includes edits and deletions)
      - `message.im` – Receive direct messages (includes edits and deletions)
      - `message.mpim` – Receive group direct messages (includes edits and deletions)
      - `user_typing` – (Optional) Show typing indicators
    - **Note**: Message edits and deletions are automatically included as subtypes of the message events above
    - **Note**: When using Socket Mode, you do NOT need to provide a Request URL
    - **Important**: After adding events, you must **reinstall the app** to your workspace
7. Install (or reinstall) the app to your workspace
8. Copy your **Bot Token** or **User Token** and **App Token**

## Quick Start
```bash
# Clone the repository
git clone <repo> slack_rust
cd slack_rust

# Build the release version (optimized for performance)
cargo build --release
# Or use the build script
./build.sh

# Run the client
./target/release/slack_client_rs
```

On first run, you'll be prompted to enter:
- Your **Workspace Name** (for easy identification)
- Your **Bot Token** (starts with `xoxb-`) or **User Token** (starts with `xoxp-`)
- Your **App Token** (starts with `xapp-`)

You can add more workspaces by editing the configuration file or using the setup process again.

Configuration files are stored in `~/.config/slack_client_rs/`:
- `slack_config.json` – Your workspaces, tokens and settings
- `layout.json` – Saved pane layout and open channels
- `aliases.json` – Your custom command aliases

## Usage Guide

### Navigation Basics
- **Tab** – Switch between channel list and panes, or cycle through panes
- **↑/↓** – Navigate in channel list, or move cursor in input (scroll when input is empty)
- **PageUp/PageDown** – Scroll messages faster (10 lines at a time)
- **Home/End** – Move cursor to start/end of the current input line
- **Ctrl+Home/Ctrl+End** – Jump to oldest/newest message
- **Left/Right** – Move cursor within the input line
- **Delete/Backspace** – Delete character forward/backward in input
- **Enter** – Open selected channel (in list) or send message (in pane)
- **Shift+Enter** – Insert newline in input
- **Esc** – Cancel reply or clear error messages

**Note**: Scrolling only works when focus is on a pane (not on the channel list). Press **Tab** to switch focus from the channel list to your active pane.

### Managing Your Workspace
- **Ctrl+N** – Show workspace list
- **Ctrl+1** through **Ctrl+9** – Switch to workspace 1-9
- **Ctrl+V** – Split current pane vertically
- **Ctrl+B** – Split current pane horizontally  
- **Ctrl+K** – Toggle split direction (horizontal ↔ vertical)
- **Ctrl+W** – Close the focused pane
- **Ctrl+L** – Clear messages in the focused pane
- **Ctrl+S** – Toggle channel list visibility

### Display Options
- **Ctrl+E** – Toggle emoji reactions display
- **Ctrl+O** – Toggle emoji rendering
- **Ctrl+T** – Toggle message timestamps
- **Ctrl+G** – Toggle message line numbers
- **Ctrl+D** – Toggle compact mode (reduced spacing)
- **Ctrl+U** – Toggle color-coded usernames
- **Ctrl+Y** – Toggle borders (for cleaner UI)

### System Commands
- **Ctrl+R** – Refresh channel list
- **Ctrl+Q** – Quit (state is automatically saved)

## Commands Reference

All commands start with `/` and are typed in the message input area. Some commands have short aliases for faster access.

### Message Interaction
```
/react <emoji> [msg#]
```
Add an emoji reaction to a message. If no message number is specified, reacts to the last message.
- **Example**: `/react thumbsup` – React to the last message with 👍
- **Example**: `/react heart 5` – React to message #5 with ❤️
- **Tip**: Use emoji names without colons (e.g., `thumbsup` not `:thumbsup:`)

```
/thread <msg#>
/t <msg#>
```
Open a message thread in a new pane. The parent message and all replies will be displayed.
- **Example**: `/thread 3` – Open thread for message #3
- **Example**: `/t 7` – Open thread for message #7 (short form)

### Filtering Messages
```
/filter [type] [value]
/filter
```
Filter messages in the current pane by different criteria. Use `/filter` without arguments to clear the filter.

**Filter Types**:
- **sender** – Show only messages from a specific user
  - Example: `/filter sender John`
- **media** – Show only messages with media attachments
  - Example: `/filter media`
- **link** – Show only messages containing links
  - Example: `/filter link`

To clear all filters: `/filter`

### Custom Aliases
```
/alias <name> <value>
```
Create a custom alias that expands to a longer text. Useful for frequently used phrases or commands.
- **Example**: `/alias brb Be right back!`
- **Example**: `/alias meeting In a meeting, will respond later`
- **Usage**: Type `brb` in your message and it expands automatically

```
/unalias <name>
```
Remove an existing alias.
- **Example**: `/unalias brb`

### Workspace Management
```
/workspace [name|number]
/ws [name|number]
```
Switch to a different workspace or show the list of all workspaces.
- **Example**: `/workspace` – Show all configured workspaces
- **Example**: `/workspace 2` – Switch to workspace #2
- **Example**: `/ws MyCompany` – Switch to workspace named "MyCompany"
- **Tip**: Use `Ctrl+1` through `Ctrl+9` for quick switching

### Channel Management
```
/leave
```
Leave the current channel. You'll be removed from the channel and the pane will close.

### Help
```
/help
/h
```
Display help information about available commands.

## Workflow Examples

### Multi-Channel Monitoring
1. Open your main work channel
2. Press `Ctrl+V` to split vertically
3. Navigate to another channel with `Tab` + arrow keys, then `Enter`
4. Repeat to monitor multiple channels simultaneously
5. Click any pane or use `Tab` to switch focus
6. Your layout and open channels are saved when you quit

### Thread Conversations
1. View a message in a channel (note the line number)
2. Type `/t <number>` to open the thread in a new pane
3. Respond directly in the thread pane
4. Close with `Ctrl+W` when done

### Quick Reactions
1. Find a message you want to react to (note the line number)
2. Type `/react <emoji> <number>` to add your reaction
3. Or just `/react <emoji>` to react to the most recent message
4. Toggle reaction display with `Ctrl+E` if the pane gets cluttered

### Focused Reading with Filters
1. Open a busy channel
2. Type `/filter sender Alice` to see only Alice's messages
3. Or `/filter media` to see only messages with attachments
4. Type `/filter` to clear and see all messages again

### Efficient Text with Aliases
1. Create common responses: `/alias ooo Out of office until tomorrow`
2. Use in messages: Type `ooo` and it expands automatically
3. Manage aliases: `/unalias ooo` to remove

## Technical Details

### Architecture
The application is structured into several modules:

- **main.rs** – Entry point, terminal setup, and main event loop
- **app.rs** – Core application state, UI rendering, and pane management
- **slack.rs** – Slack API integration (HTTP + Socket Mode WebSocket)
- **widgets.rs** – Chat pane data structures and message formatting
- **split_view.rs** – Binary tree layout for pane splitting
- **commands.rs** – Command parser and handlers
- **formatting.rs** – Message text formatting and emoji rendering
- **persistence.rs** – State saving/loading (layout, aliases, settings)
- **config.rs** – Configuration file management
- **utils.rs** – Utility functions (notifications, etc.)

### State Management
- Each pane maintains independent state (scroll position, input buffer, filters)
- Format caching for efficient re-rendering of large message histories
- Auto-save on exit or `Ctrl+Q` to preserve your workspace

### Performance
- Efficient event polling (50ms) balances responsiveness with CPU usage
- Message format caching prevents redundant text processing
- Selective rendering only updates visible content

### Data Storage
Configuration directory: `~/.config/slack_client_rs/`
- `slack_config.json` – Workspaces with tokens and settings
- `layout.json` – Pane tree structure and open channels
- `aliases.json` – User-defined text aliases

## Configuration File Format

The `slack_config.json` supports multiple workspaces:

```json
{
  "workspaces": [
    {
      "name": "My Company",
      "token": "xoxp-...",
      "app_token": "xapp-..."
    },
    {
      "name": "Side Project",
      "token": "xoxp-...",
      "app_token": "xapp-..."
    }
  ],
  "active_workspace": 0,
  "settings": {
    "show_reactions": true,
    "show_notifications": true,
    "compact_mode": false,
    "show_emojis": true,
    "show_line_numbers": false,
    "show_timestamps": true,
    "show_chat_list": true,
    "show_user_colors": true,
    "show_borders": true
  }
}
```

The client automatically converts old single-workspace configs to the new format.

## Troubleshooting

### Connection Issues
- Verify your tokens are correct in `~/.config/slack_client_rs/slack_config.json`
- Check that Socket Mode is enabled in your Slack app settings
- Ensure your app has the necessary OAuth scopes

### Messages Not Appearing
- Press `Ctrl+R` to manually refresh the channel list
- Check that your app/bot is added to private channels
- Verify the app has `channels:history` and `groups:history` scopes

### Keyboard Shortcuts Not Working
- Ensure your terminal passes through Ctrl key combinations
- Some terminals may intercept certain shortcuts – check your terminal settings
- Try the alternate command format if available (e.g., `/h` instead of `/help`)

### Display Issues
- Increase terminal size for better layout with multiple panes
- Toggle `Ctrl+D` for compact mode if messages are too spaced out
- Use `Ctrl+S` to hide the channel list for more message space

## Contributing

Contributions are welcome! Areas for improvement:
- Additional message formatting support (code blocks, quotes, etc.)
- More filtering options (date ranges, keywords, etc.)
- Search functionality across messages
- Direct message group management
- File upload support
- Custom themes and color schemes

## Project Layout
```
src/
├── main.rs           # Entry point + event loop
├── app.rs            # Core application + UI rendering
├── slack.rs          # Slack API (HTTP + Socket Mode)
├── widgets.rs        # Chat pane data structures
├── split_view.rs     # Layout tree for pane splitting
├── commands.rs       # Command parsing + handlers
├── formatting.rs     # Message text formatting
├── persistence.rs    # State saving/loading
├── config.rs         # Configuration management
└── utils.rs          # Utility functions

config/
├── slack_config.json # Tokens and workspace
├── layout.json       # Saved pane layout
└── aliases.json      # Custom text aliases
```

## License

This project is open source.
