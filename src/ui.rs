//! Top-level rendering: sidebar (explorer) + editor area (tabs + content) + statusline.
//! Phase 5 replaces the fixed sidebar|editor split with the recursive layout tree.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::theme::DARK;

pub const SIDEBAR_WIDTH: u16 = 32;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    if app.sidebar_visible {
        let [side, edit] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
                .areas(main);
        draw_sidebar(frame, app, side);
        draw_editor(frame, app, edit);
    } else {
        draw_editor(frame, app, main);
    }
    draw_status(frame, app, status);
}

fn border_color(focused: bool) -> Color {
    if focused {
        DARK.accent
    } else {
        DARK.border
    }
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color(focused)))
        .title(Span::styled(
            " Explorer ",
            Style::default().fg(DARK.accent).bold(),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let width = inner.width as usize;
    app.tree.ensure_visible(height);
    let scroll = app.tree.scroll;

    let lines: Vec<Line> = app
        .tree
        .rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, row)| {
            let indent = "  ".repeat(row.depth);
            let icon = if row.is_dir {
                if row.expanded {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            let label = format!("{indent}{icon}{}", row.name);
            if i == app.tree.selected {
                // Pad to full width so the selection bar spans the pane.
                let style = if focused {
                    Style::default().bg(DARK.accent).fg(Color::Black)
                } else {
                    Style::default().bg(DARK.status_bg).fg(DARK.fg)
                };
                Line::from(Span::styled(format!("{label:<width$}"), style))
            } else {
                let style = if row.is_dir {
                    Style::default().fg(DARK.accent)
                } else {
                    Style::default().fg(DARK.fg)
                };
                Line::from(Span::styled(label, style))
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_editor(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Editor;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color(focused)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.editor.tabs.is_empty() {
        app.editor.viewport = 0;
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No file open",
                Style::default().fg(DARK.fg).bold(),
            )),
            Line::from(Span::styled(
                "Pick a file in the explorer and press Enter.",
                Style::default().fg(DARK.dim),
            )),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    }

    let [tabbar, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    // Render tabs manually so each tab's screen x-range is known for click mapping.
    let labels: Vec<(usize, String)> = app
        .editor
        .tabs
        .iter()
        .enumerate()
        .map(|(i, b)| (i, format!(" {} ", b.name)))
        .collect();
    let active = app.editor.active;
    let mut spans = Vec::new();
    let mut hitboxes = Vec::new();
    let mut x = tabbar.x;
    for (i, label) in &labels {
        let w = label.chars().count() as u16;
        let style = if *i == active {
            Style::default().fg(DARK.accent).bold()
        } else {
            Style::default().fg(DARK.dim)
        };
        spans.push(Span::styled(label.clone(), style));
        hitboxes.push((x, x + w, *i));
        x += w;
    }
    app.editor.tab_hitboxes = hitboxes;
    app.editor.tabbar_row = tabbar.y;
    frame.render_widget(Paragraph::new(Line::from(spans)), tabbar);

    app.editor.viewport = content.height as usize;
    app.editor.content_area = content;
    if let Some(buf) = app.editor.active_buffer() {
        let start = buf.scroll.min(buf.line_count().saturating_sub(1));
        let end = (start + content.height as usize).min(buf.line_count());
        let visible: Vec<Line> = buf.lines[start..end].to_vec();
        frame.render_widget(Paragraph::new(visible), content);
    }
    draw_selection(frame, app, content);
}

/// Overlay the selection background on the selected cells, leaving the syntax-highlighted
/// foreground intact (a bg-only style patch).
fn draw_selection(frame: &mut Frame, app: &App, content: Rect) {
    let Some(sel) = app.editor.selection else {
        return;
    };
    let Some(buf) = app.editor.active_buffer() else {
        return;
    };
    let ((sl, sc), (el, ec)) = sel.normalized();
    let scroll = buf.scroll;
    let height = content.height as usize;
    let style = Style::default().bg(DARK.sel_bg);
    for li in sl..=el {
        if li < scroll || li >= scroll + height {
            continue;
        }
        let line_len = buf.line_text(li).chars().count();
        let start = if li == sl { sc } else { 0 }.min(line_len);
        let end = if li == el { ec } else { line_len }.min(line_len);
        if end <= start {
            continue;
        }
        let y = content.y + (li - scroll) as u16;
        let x = content.x + start as u16;
        let max_w = content.width.saturating_sub(start as u16);
        let w = ((end - start) as u16).min(max_w);
        if w > 0 {
            frame.buffer_mut().set_style(Rect::new(x, y, w, 1), style);
        }
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let km = &app.config.keymap;
    let bg = Style::default().bg(DARK.status_bg).fg(DARK.status_fg);

    let focus = match app.focus {
        Focus::Sidebar => "EXPLORER",
        Focus::Editor => "EDITOR",
    };
    let left = Line::from(vec![
        Span::styled(format!(" {focus} "), bg.fg(DARK.accent).bold()),
        Span::styled(format!("{} ", app.status), bg),
    ]);
    let right = Line::from(Span::styled(
        format!(
            " {} sidebar · {} focus · {} quit ",
            km.toggle_sidebar, km.focus_next, km.quit
        ),
        bg,
    ));

    let [l, r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)])
            .areas(area);
    frame.render_widget(Paragraph::new(left).style(bg), l);
    frame.render_widget(Paragraph::new(right).style(bg).alignment(Alignment::Right), r);
}
