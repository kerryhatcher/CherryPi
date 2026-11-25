use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::{App, ViewMode, MessageKind, ToolCallDisplay};
use super::theme::Theme;
use cherrypi_core::message::MessageRole;

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &mut App) {
    let theme = app.theme.clone();

    // Main layout: status bar at bottom, content fills rest
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),       // Content area
            Constraint::Length(1),    // Status bar
        ])
        .split(frame.area());

    // Render content based on view mode
    match app.view_mode {
        ViewMode::Chat => render_chat(frame, main_chunks[0], app, &theme),
        ViewMode::FileViewer { ref path, ref content } => {
            render_file_viewer(frame, main_chunks[0], path, content, &theme)
        }
        ViewMode::PluginManager => render_plugin_manager(frame, main_chunks[0], app, &theme),
        ViewMode::ModelSelector => render_model_selector(frame, main_chunks[0], app, &theme),
        ViewMode::Help => render_help(frame, main_chunks[0], &theme),
    }

    // Render status bar
    render_status_bar(frame, main_chunks[1], app, &theme);
}

fn render_chat(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    // Determine if we need a suggestions row
    let has_suggestions = !app.suggestions.is_empty();
    let suggestion_height = if has_suggestions {
        (app.suggestions.len() as u16).min(5) + 2 // +2 for borders
    } else {
        0
    };

    let constraints: Vec<Constraint> = if has_suggestions {
        vec![
            Constraint::Min(3),                    // Chat messages
            Constraint::Length(suggestion_height),  // Suggestions
            Constraint::Length(3),                  // Input area
        ]
    } else {
        vec![
            Constraint::Min(3),       // Chat messages
            Constraint::Length(3),    // Input area
        ]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_messages(frame, chunks[0], app, theme);

    if has_suggestions {
        render_suggestions(frame, chunks[1], app, theme);
        render_input(frame, chunks[2], app, theme);
    } else {
        render_input(frame, chunks[1], app, theme);
    }
}

fn render_messages(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" CherryPi Chat ")
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build message lines
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        match &msg.kind {
            MessageKind::ToolCall(tc) => {
                render_tool_call_box(&mut lines, tc, theme);
            }
            MessageKind::Normal => {
                let (role_color, prefix) = match msg.role {
                    MessageRole::User => (theme.user_msg, "You"),
                    MessageRole::Assistant => (theme.assistant_msg, "CherryPi"),
                    MessageRole::System => (theme.system_msg, "System"),
                    MessageRole::Tool => (theme.accent_light, "Tool"),
                };

                // Role header
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", prefix),
                        Style::default()
                            .fg(role_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));

                // Message content (split by newlines)
                for content_line in msg.content.lines() {
                    let style = if msg.is_error {
                        Style::default().fg(theme.error)
                    } else {
                        Style::default().fg(theme.fg)
                    };

                    // Simple syntax highlighting for code blocks
                    if content_line.starts_with("```") || content_line.starts_with("    ") {
                        lines.push(Line::from(Span::styled(
                            content_line,
                            Style::default().fg(theme.accent_light),
                        )));
                    } else {
                        lines.push(Line::from(Span::styled(content_line, style)));
                    }
                }
            }
        }

        // Add spacing between messages
        lines.push(Line::from(""));
    }

    let text = Text::from(lines);

    // Calculate virtual (wrapped) line count for accurate scroll clamping.
    // Paragraph::scroll() with Wrap operates on post-wrap lines, but
    // text.lines.len() is the pre-wrap count.  Long lines get word-wrapped
    // at inner.width, making the virtual count much larger.
    let available_width = inner.width.max(1) as usize;
    let virtual_line_count: usize = text.lines.iter()
        .map(|line| {
            let w = line.width();
            if w == 0 {
                1
            } else {
                w.div_ceil(available_width).max(1)
            }
        })
        .sum();

    // Clamp scroll: usize::MAX means "stick to bottom"
    let max_scroll = virtual_line_count.saturating_sub(inner.height as usize);
    if app.chat_scroll == usize::MAX || app.chat_scroll > max_scroll {
        app.chat_scroll = max_scroll;
    }
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((app.chat_scroll as u16, 0));

    frame.render_widget(paragraph, inner);
}

/// Render a tool call as a styled box within the message stream
fn render_tool_call_box(lines: &mut Vec<Line>, tc: &ToolCallDisplay, theme: &Theme) {
    let border_style = if tc.is_error {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.tool_call_border)
    };
    let header_style = Style::default()
        .fg(theme.tool_call_header)
        .add_modifier(Modifier::BOLD);
    let input_label_style = Style::default()
        .fg(theme.tool_call_input_label)
        .add_modifier(Modifier::BOLD);
    let output_label_style = Style::default()
        .fg(if tc.is_error { theme.error } else { theme.tool_call_output_label })
        .add_modifier(Modifier::BOLD);
    let output_style = if tc.is_error {
        Style::default().fg(theme.error)
    } else {
        Style::default().fg(theme.fg)
    };

    // Top border: ┌─ ⚙ tool_name ─────────────────────┐
    lines.push(Line::from(vec![
        Span::styled("┌─ ⚙ ", border_style),
        Span::styled(tc.tool_name.clone(), header_style),
        Span::styled(" ", border_style),
    ]));

    // Input section
    lines.push(Line::from(vec![
        Span::styled("│ ", border_style),
        Span::styled("▸ Input:", input_label_style),
    ]));

    // Input content (indented)
    for input_line in tc.tool_input.lines() {
        lines.push(Line::from(vec![
            Span::styled("│ ", border_style),
            Span::styled(input_line.to_string(), Style::default().fg(theme.accent_light)),
        ]));
    }

    // Separator and output section
    lines.push(Line::from(vec![
        Span::styled("│", border_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled("│ ", border_style),
        Span::styled("▸ Result:", output_label_style),
    ]));

    // Output content
    for output_line in tc.tool_output.lines() {
        lines.push(Line::from(vec![
            Span::styled("│ ", border_style),
            Span::styled(output_line.to_string(), output_style),
        ]));
    }

    // Bottom border
    lines.push(Line::from(vec![
        Span::styled("└──────────────────────────────────┘", border_style),
    ]));
}

fn render_input(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let spinner = if app.is_processing {
        format!(" {} ", app.spinner_char())
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.is_processing {
            theme.warning
        } else {
            theme.border_focused
        }))
        .title(if app.is_processing {
            format!(" {} Thinking... (Esc to cancel) ", spinner)
        } else {
            " Input (Enter to send, Shift+Enter for newline) ".to_string()
        })
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input_text = if app.input.is_empty() && !app.is_processing {
        Text::from(Span::styled(
            "Type your message...",
            Style::default().fg(theme.line_number),
        ))
    } else {
        Text::from(Span::styled(
            &app.input,
            Style::default().fg(theme.input_fg),
        ))
    };

    let paragraph = Paragraph::new(input_text).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn render_suggestions(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Commands ")
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = app.suggestions
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let is_selected = i == app.suggestion_index;
            let style = if is_selected {
                Style::default().fg(theme.bg).bg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", cmd.name),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("— {}", cmd.description),
                    Style::default().fg(theme.line_number),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::default().bg(theme.accent).fg(theme.bg));

    frame.render_widget(list, inner);
}

fn render_file_viewer(
    frame: &mut Frame,
    area: Rect,
    path: &str,
    content: &str,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(format!(" File: {} ", path))
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            Line::from(vec![
                Span::styled(
                    format!("{:4} ", i + 1),
                    Style::default().fg(theme.line_number),
                ),
                Span::styled(line, Style::default().fg(theme.fg)),
            ])
        })
        .collect();

    let text = Text::from(lines);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });

    frame.render_widget(paragraph, inner);
}

fn render_plugin_manager(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Plugin Manager ")
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = if app.plugin_list.is_empty() {
        vec![ListItem::new(Span::styled(
            "No plugins loaded. Place plugins in ~/.cherrypi/plugins/",
            Style::default().fg(theme.line_number),
        ))]
    } else {
        app.plugin_list
            .iter()
            .map(|name| {
                ListItem::new(Span::styled(
                    format!("  📦 {}", name),
                    Style::default().fg(theme.fg),
                ))
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::default().bg(theme.accent).fg(theme.bg))
        .highlight_symbol("> ");

    app.plugin_list_state.select(Some(app.list_selection));
    frame.render_stateful_widget(list, inner, &mut app.plugin_list_state);
}

fn render_model_selector(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Model Selector ")
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = if app.available_models.is_empty() {
        vec![ListItem::new(Span::styled(
            "Loading models...",
            Style::default().fg(theme.line_number),
        ))]
    } else {
        app.available_models
            .iter()
            .map(|name| {
                let is_current = name == &app.current_model;
                let label = if is_current {
                    format!("{} (current)", name)
                } else {
                    name.clone()
                };
                ListItem::new(Span::styled(
                    format!("  {}", label),
                    Style::default().fg(theme.fg),
                ))
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::default().bg(theme.accent).fg(theme.bg).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    app.model_list_state.select(Some(app.list_selection));
    frame.render_stateful_widget(list, inner, &mut app.model_list_state);
}

fn render_help(frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(" Help ")
        .title_style(Style::default().fg(theme.accent));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let help_text = super::app::HELP_TEXT;
    let paragraph = Paragraph::new(help_text)
        .style(Style::default().fg(theme.fg))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, inner);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let status = Block::default()
        .style(Style::default().bg(theme.status_bar_bg).fg(theme.status_bar_fg));

    let inner = status.inner(area);
    frame.render_widget(status, area);

    let spinner = if app.is_processing {
        format!("{} ", app.spinner_char())
    } else {
        String::new()
    };

    let plugin_segments: Vec<_> = app
        .status_bar_manager
        .segments()
        .into_iter()
        .filter(|segment| !segment.text.is_empty())
        .collect();

    let spans: Vec<Span> = if plugin_segments.is_empty() {
        // Built-in status is only a fallback when no plugin provides output.
        vec![Span::styled(
            format!(" {}{} │ {} ", spinner, app.status_message, app.current_model),
            Style::default().fg(theme.status_bar_fg),
        )]
    } else {
        // A status-bar plugin owns the entire line. Multiple plugins, if
        // configured, are separated without adding CherryPi metadata.
        let mut spans = vec![Span::styled(
            " ",
            Style::default().fg(theme.status_bar_fg),
        )];
        for (index, segment) in plugin_segments.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(
                    " │ ",
                    Style::default().fg(theme.status_bar_fg),
                ));
            }
            if let Some(ref label) = segment.label {
                spans.push(Span::styled(
                    format!("{}: ", label),
                    Style::default().fg(theme.status_bar_fg),
                ));
            }
            let seg_style = if segment.healthy {
                Style::default().fg(theme.status_bar_fg)
            } else {
                Style::default().fg(theme.error)
            };
            spans.push(Span::styled(&segment.text, seg_style));
        }
        spans
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}
