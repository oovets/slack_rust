use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;

mod app;
mod commands;
mod config;
mod formatting;
mod persistence;
mod slack;
mod split_view;
mod utils;
mod widgets;

use app::App;

#[tokio::main]
async fn main() -> Result<()> {
    // Create app BEFORE entering TUI mode (so authentication can work)
    let mut app = App::new().await?;
    
    // Load chat history for saved panes
    let _ = app.load_all_pane_histories().await;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?; // Cursor shown only when input is focused

    // Run app
    let _res = run_app(&mut terminal, &mut app).await;

    // Save state before exiting (even if there was an error)
    let _ = app.save_state();

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| app.draw(f))?;

        // Process Slack events FIRST - check for new messages frequently
        app.process_slack_events().await?;

        // Handle pending chat open (from mouse click)
        if app.pending_open_chat {
            app.pending_open_chat = false;
            app.open_selected_chat().await?;
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            let event = event::read()?;
            match event {
                Event::Key(key) => {
                    match key.code {
                        // Ctrl+Q: Quit
                        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.save_state()?;
                            break;
                        }
                        // Ctrl+R: Refresh chats
                        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.refresh_chats().await?;
                        }
                        // Ctrl+V: Split vertical
                        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.split_vertical();
                        }
                        // Ctrl+B: Split horizontal
                        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.split_horizontal();
                        }
                        // Ctrl+K: Toggle split direction
                        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_split_direction();
                        }
                        // Ctrl+W: Close pane
                        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.close_pane();
                        }
                        // Ctrl+S: Toggle chat list (Sidebar)
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_chat_list();
                        }
                        // Ctrl+L: Clear pane
                        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.clear_pane();
                        }
                        // Ctrl+E: Toggle reactions
                        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_reactions();
                        }
                        // Ctrl+O: Toggle emojis
                        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_emojis();
                        }
                        // Ctrl+T: Toggle timestamps
                        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_timestamps();
                        }
                        // Ctrl+D: Toggle compact mode
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_compact_mode();
                        }
                        // Ctrl+G: Toggle line numbers
                        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_line_numbers();
                        }
                        // Ctrl+U: Toggle user colors
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_user_colors();
                        }
                        // Ctrl+Y: Toggle borders
                        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.toggle_borders();
                        }
                        // Tab: Next pane / Switch to chat list
                        KeyCode::Tab => {
                            if !app.focus_on_chat_list
                                && !app.panes[app.focused_pane_idx].input_buffer.is_empty()
                            {
                                app.tab_complete();
                            } else {
                                app.next_pane();
                            }
                        }
                        // Enter: Send message (when focus on chat pane)
                        KeyCode::Enter if !app.focus_on_chat_list => {
                            app.send_message().await?;
                        }
                        // Enter: Open chat (when focus on chat list)
                        KeyCode::Enter if app.focus_on_chat_list => {
                            app.open_selected_chat().await?;
                        }
                        // Up/Down: Navigate pane or chat list
                        KeyCode::Up => {
                            if app.focus_on_chat_list {
                                app.select_previous_chat();
                            } else {
                                app.scroll_up();
                            }
                        }
                        KeyCode::Down => {
                            if app.focus_on_chat_list {
                                app.select_next_chat();
                            } else {
                                app.scroll_down();
                            }
                        }
                        // Page Up/Down: Scroll faster
                        KeyCode::PageUp => {
                            if !app.focus_on_chat_list {
                                app.page_up();
                            }
                        }
                        KeyCode::PageDown => {
                            if !app.focus_on_chat_list {
                                app.page_down();
                            }
                        }
                        // Home/End: Jump to top/bottom
                        KeyCode::Home => {
                            if !app.focus_on_chat_list {
                                app.scroll_to_top();
                            }
                        }
                        KeyCode::End => {
                            if !app.focus_on_chat_list {
                                app.scroll_to_bottom();
                            }
                        }
                        // Backspace: Delete character
                        KeyCode::Backspace if !app.focus_on_chat_list => {
                            app.backspace();
                        }
                        // Esc: Clear input or cancel reply
                        KeyCode::Esc => {
                            app.cancel_reply();
                        }
                        // Character input (only when no control modifier)
                        KeyCode::Char(c) if !app.focus_on_chat_list && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.input_char(c);
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse_event) => {
                    use crossterm::event::MouseEventKind;
                    match mouse_event.kind {
                        MouseEventKind::Down(_) => {
                            app.handle_mouse_click(mouse_event.column, mouse_event.row);
                        }
                        MouseEventKind::ScrollUp => {
                            let in_chat_list = app.chat_list_area.map_or(false, |area| {
                                mouse_event.column >= area.x
                                    && mouse_event.column < area.x + area.width
                                    && mouse_event.row >= area.y
                                    && mouse_event.row < area.y + area.height
                            });
                            if in_chat_list {
                                app.select_previous_chat();
                            } else {
                                app.scroll_up();
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            let in_chat_list = app.chat_list_area.map_or(false, |area| {
                                mouse_event.column >= area.x
                                    && mouse_event.column < area.x + area.width
                                    && mouse_event.row >= area.y
                                    && mouse_event.row < area.y + area.height
                            });
                            if in_chat_list {
                                app.select_next_chat();
                            } else {
                                app.scroll_down();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}
