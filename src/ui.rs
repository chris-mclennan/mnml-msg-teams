//! ratatui rendering + the main event loop.

use crate::app::{App, DetailKind, Item, TabState, short_ts};
use crate::keys;
use crate::teams::Message;
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};
use std::collections::HashMap;
use std::io::Stdout;
use std::time::Duration;

pub fn run(app: &mut App) -> Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.tick();
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && let Some(action) = keys::handle(key, app)
        {
            let quit = keys::apply(action, app);
            if quit {
                break;
            }
        }
    }
    Ok(())
}

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(size);
    draw_tabs(f, chunks[0], app);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);
    draw_list(f, body[0], app.active());
    draw_detail(f, body[1], app);
    draw_status(f, chunks[2], app);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let labels: Vec<Line> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let badge = if t.data.loading {
                " (…)".to_string()
            } else if t.data.last_error.is_some() {
                " (err)".to_string()
            } else {
                format!(" ({})", t.data.items.len())
            };
            Line::from(format!("{}.{}{}", i + 1, t.name, badge))
        })
        .collect();
    let tabs = Tabs::new(labels)
        .block(Block::default().borders(Borders::ALL).title(" teams "))
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn draw_list(f: &mut Frame, area: Rect, tab: &TabState) {
    if let Some(err) = &tab.data.last_error {
        let p = Paragraph::new(format!("error: {err}"))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }
    if tab.data.items.is_empty() {
        let msg = if tab.data.loading {
            "(loading…)"
        } else {
            "(none)"
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" items "));
        f.render_widget(p, area);
        return;
    }

    // Search-tab query line takes the top row when in search-mode.
    let mut top_lines: Vec<Line> = Vec::new();
    if tab.data.search_mode {
        top_lines.push(Line::from(vec![
            Span::styled(" search: ", Style::default().fg(Color::Yellow)),
            Span::raw(tab.data.search_query.clone()),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
        ]));
    }

    let body_rows = area.height.saturating_sub(2 + top_lines.len() as u16) as usize;
    let total = tab.data.items.len();
    let selected = tab.data.selected;
    let start = if total <= body_rows {
        0
    } else {
        let lo = selected.saturating_sub(body_rows / 2);
        lo.min(total - body_rows)
    };

    let mut lines: Vec<Line> = top_lines;
    for (i, item) in tab.data.items[start..].iter().take(body_rows).enumerate() {
        let abs = start + i;
        let cursor = if abs == selected { "▸ " } else { "  " };
        let primary = truncate(&item.primary_label(), 36);
        let secondary = truncate(&item.secondary_label(), 80);
        let line = if secondary.is_empty() {
            format!("{cursor}{:<36}", primary)
        } else {
            format!("{cursor}{:<36}  {secondary}", primary)
        };
        let style = if abs == selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            row_style(item)
        };
        lines.push(Line::from(Span::styled(line, style)));
    }

    let title = match tab.spec.kind.as_str() {
        "teams" => format!(" teams ({total}) "),
        "chats" => format!(" chats ({total}) "),
        "search" => format!(" search ({total}) "),
        "threads" => " threads ".to_string(),
        _ => format!(" items ({total}) "),
    };
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn row_style(item: &Item) -> Style {
    match item {
        Item::Team { .. } => Style::default().fg(Color::White),
        Item::Channel { .. } => Style::default().fg(Color::Gray),
        Item::Chat(_) => Style::default().fg(Color::White),
        Item::Message(_) => Style::default().fg(Color::Gray),
        Item::SearchPrompt => Style::default().fg(Color::DarkGray),
        Item::Placeholder(_) => Style::default().fg(Color::DarkGray),
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let tab = app.active();

    // If composing, show the post buffer.
    if let Some(mode) = &app.post_mode {
        let title = match mode {
            crate::app::PostMode::Channel { .. } => " post → channel ",
            crate::app::PostMode::Chat { .. } => " post → chat ",
            crate::app::PostMode::ChannelReply { .. } => " thread reply ",
        };
        let p = Paragraph::new(format!(
            "{}\n\n{}▏",
            instructions_for_post(),
            app.post_buffer
        ))
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }

    let title = match &tab.data.detail_kind {
        DetailKind::Channel(_, _) => " channel ",
        DetailKind::Chat(_) => " chat ",
        DetailKind::Message => " message ",
        DetailKind::None => " detail ",
    };

    if tab.data.detail_messages.is_empty() {
        let hint = match tab.spec.kind.as_str() {
            "teams" => "Enter to expand a team's channels · focus a channel for scrollback",
            "chats" => "focus a chat to load recent messages",
            "search" => "/ to search messages across Teams",
            "threads" => "v0.1: thread tab is a placeholder",
            _ => "(no detail)",
        };
        let p = Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }

    // Render last ~30 messages (or whatever the API returned).
    // Each message → `HH:MM · username · body`. System messages dim.
    let lines: Vec<Line> = render_messages(&tab.data.detail_messages);

    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn instructions_for_post() -> &'static str {
    "(Ctrl+S to send, Esc to cancel — single-line v0.1)"
}

fn render_messages(msgs: &[Message]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Graph returns newest-first; reverse for chronological scrollback.
    let ordered: Vec<&Message> = msgs.iter().rev().collect();
    for m in ordered {
        if m.is_system() {
            let body = strip_to_one_line(&m.body_text());
            lines.push(Line::from(Span::styled(
                format!(
                    " {} · (system) {}",
                    m.created_date_time
                        .as_deref()
                        .map(short_ts)
                        .unwrap_or_default(),
                    body
                ),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }
        let ts = m
            .created_date_time
            .as_deref()
            .map(short_ts)
            .unwrap_or_default();
        let author = m.author();
        let body = m.body_text();
        let resolved = resolve_mentions(&body);

        let header = Line::from(vec![
            Span::styled(format!(" {ts} "), Style::default().fg(Color::DarkGray)),
            Span::styled("· ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                author,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        lines.push(header);

        for ln in resolved.lines().take(8) {
            lines.push(Line::from(Span::styled(
                format!("   {ln}"),
                Style::default().fg(Color::White),
            )));
        }

        // Reactions chips
        if !m.reactions.is_empty() {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for r in &m.reactions {
                if let Some(rt) = &r.reaction_type {
                    *counts.entry(rt.as_str()).or_insert(0) += 1;
                }
            }
            let mut chip_spans: Vec<Span<'static>> = vec![Span::raw("   ")];
            for (rt, n) in counts.iter() {
                let glyph = reaction_glyph(rt);
                chip_spans.push(Span::styled(
                    format!("[{glyph} {n}] "),
                    Style::default().fg(Color::Yellow),
                ));
            }
            lines.push(Line::from(chip_spans));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn reaction_glyph(rt: &str) -> &'static str {
    match rt {
        "like" => "👍",
        "heart" => "❤",
        "laugh" => "😂",
        "surprised" => "😮",
        "sad" => "😢",
        "angry" => "😡",
        _ => "•",
    }
}

fn strip_to_one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    truncate(first, 80)
}

/// Resolve `<at id="...">@Display Name</at>` spans inline. Best-effort
/// — when the body is already plain text (or HTML was already
/// stripped) the regex won't match and the string passes through.
fn resolve_mentions(body: &str) -> String {
    // After strip_html, `<at>` tags are already gone. As a safety
    // net, re-strip any leftover `<at>...</at>` shapes.
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            while let Some(&p) = chars.peek() {
                if p == '>' {
                    chars.next();
                    break;
                }
                if tag.len() < 8 {
                    tag.push(p);
                }
                chars.next();
            }
            // drop the tag silently
            let _ = tag;
        } else {
            out.push(c);
        }
    }
    out
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let hint = " 1-9 tab · ↑↓/jk move · Enter open · / search · p post · R react · T thread · y permalink · r refresh · q quit ";
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn reaction_glyphs_known() {
        assert_eq!(reaction_glyph("like"), "👍");
        assert_eq!(reaction_glyph("heart"), "❤");
        assert_eq!(reaction_glyph("unknown"), "•");
    }
}
