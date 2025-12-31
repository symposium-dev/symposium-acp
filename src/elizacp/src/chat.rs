//! Simple TUI chat interface for Eliza.
//!
//! Run with: `cargo run -p elizacp -- chat`

use crate::eliza::Eliza;
use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io::{self, stdout};

/// A single message in the chat history
struct Message {
    text: String,
    is_user: bool,
}

/// Run the chat TUI
pub fn run(deterministic: bool) -> anyhow::Result<()> {
    // Setup terminal
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let result = run_app(&mut terminal, deterministic);

    // Restore terminal
    disable_raw_mode()?;
    stdout().execute(DisableMouseCapture)?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    deterministic: bool,
) -> anyhow::Result<()> {
    let mut eliza = if deterministic {
        Eliza::new_deterministic()
    } else {
        Eliza::new()
    };
    let mut messages: Vec<Message> = vec![Message {
        text: "Hello, I am Eliza. How can I help you today?".to_string(),
        is_user: false,
    }];
    let mut input = String::new();
    let mut scroll_offset: i32 = 0; // negative = scrolled up from bottom

    loop {
        // Draw UI
        terminal.draw(|frame| {
            let area = frame.area();

            // Split into messages area and input area
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(area);

            // Messages area
            let messages_text: String = messages
                .iter()
                .map(|m| {
                    if m.is_user {
                        format!("You: {}", m.text)
                    } else {
                        format!("Eliza: {}", m.text)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            // Calculate scroll to show latest messages with some padding
            let visible_height = chunks[0].height.saturating_sub(2) as usize; // -2 for borders
            let inner_width = chunks[0].width.saturating_sub(2) as usize; // -2 for borders

            // Count wrapped lines (approximate - each line wraps based on width)
            let wrapped_line_count: usize = messages_text
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        1
                    } else {
                        (line.len() + inner_width - 1) / inner_width
                    }
                })
                .sum();

            // Scroll so last message ends up in the middle of the visible area
            let padding = visible_height / 2;
            let auto_scroll = (wrapped_line_count + padding).saturating_sub(visible_height) as i32;
            let scroll = (auto_scroll + scroll_offset).max(0) as u16;

            let messages_paragraph = Paragraph::new(messages_text)
                .block(Block::default().borders(Borders::ALL).title("Chat"))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));

            frame.render_widget(messages_paragraph, chunks[0]);

            // Input area
            let input_paragraph = Paragraph::new(input.as_str()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Type here (Enter to send, Esc to quit)"),
            );

            frame.render_widget(input_paragraph, chunks[1]);

            // Position cursor at end of input
            frame.set_cursor_position((chunks[1].x + input.len() as u16 + 1, chunks[1].y + 1));
        })?;

        // Handle input
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Esc => break,
                        KeyCode::Enter => {
                            if !input.is_empty() {
                                // Add user message
                                messages.push(Message {
                                    text: input.clone(),
                                    is_user: true,
                                });

                                // Get Eliza's response
                                let response = eliza.respond(&input);
                                messages.push(Message {
                                    text: response,
                                    is_user: false,
                                });

                                input.clear();
                                scroll_offset = 0; // Reset scroll on new message
                            }
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(c) => {
                            input.push(c);
                        }
                        KeyCode::PageUp => {
                            scroll_offset -= 10;
                        }
                        KeyCode::PageDown => {
                            scroll_offset = (scroll_offset + 10).min(0);
                        }
                        KeyCode::Up => {
                            scroll_offset -= 1;
                        }
                        KeyCode::Down => {
                            scroll_offset = (scroll_offset + 1).min(0);
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        scroll_offset -= 3;
                    }
                    MouseEventKind::ScrollDown => {
                        scroll_offset = (scroll_offset + 3).min(0);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    Ok(())
}
