//! Top-level rendering: sidebar (explorer) + editor area (tabs + content) + statusline.
//! Phase 5 replaces the fixed sidebar|editor split with the recursive layout tree.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::prompt::Prompt;
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::theme::DARK;

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.term_width = frame.area().width;
    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    if app.sidebar_visible {
        let [side, edit] =
            Layout::horizontal([Constraint::Length(app.sidebar_width), Constraint::Min(0)])
                .areas(main);
        draw_sidebar(frame, app, side);
        draw_editor(frame, app, edit);
    } else {
        draw_editor(frame, app, main);
    }
    draw_status(frame, app, status);

    if let Some(prompt) = app.prompt.as_ref() {
        draw_prompt(frame, prompt, frame.area());
    }
    if let Some(idx) = app.close_confirm {
        if let Some(name) = app.editor.tabs.get(idx).map(|b| b.name.as_str()) {
            draw_confirm(frame, name, frame.area());
        }
    }
    if app.show_help {
        draw_help(frame, app, frame.area());
    }
}

/// A centered overlay listing every keybinding.
fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let bindings = app.config.keymap.bindings();
    let h = (bindings.len() as u16 + 2).min(area.height);
    let w = 48.min(area.width.saturating_sub(4)).max(30);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect::new(x, y, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DARK.accent))
        .title(Span::styled(
            " Keybindings — Esc to close ",
            Style::default().fg(DARK.accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let lines: Vec<Line> = bindings
        .iter()
        .map(|(label, chord)| {
            Line::from(vec![
                Span::styled(format!(" {:<14}", chord.to_string()), Style::default().fg(DARK.accent)),
                Span::styled((*label).to_string(), Style::default().fg(DARK.fg)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A centered Save / Discard / Cancel dialog for closing a modified file.
fn draw_confirm(frame: &mut Frame, name: &str, area: Rect) {
    let w = area.width.saturating_sub(8).clamp(34, 64);
    let h = 5;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 3;
    let rect = Rect::new(x, y, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DARK.accent))
        .title(Span::styled(
            " Unsaved changes ",
            Style::default().fg(DARK.accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let key = |k: &'static str| Span::styled(k, Style::default().fg(DARK.accent).bold());
    let dim = |t: &str| Span::styled(t.to_string(), Style::default().fg(DARK.dim));
    let lines = vec![
        Line::from(Span::styled(
            format!("{name} has unsaved changes."),
            Style::default().fg(DARK.fg),
        )),
        Line::from(""),
        Line::from(vec![
            key("[S]"),
            dim("ave   "),
            key("[D]"),
            dim("iscard   "),
            key("[C]"),
            dim("ancel"),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// A centered modal input box.
fn draw_prompt(frame: &mut Frame, prompt: &Prompt, area: Rect) {
    let w = area.width.saturating_sub(8).clamp(20, 60);
    let h = 3;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 3;
    let rect = Rect::new(x, y, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DARK.accent))
        .title(Span::styled(
            format!(" {} ", prompt.title),
            Style::default().fg(DARK.accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(prompt.input.as_str()).style(Style::default().fg(DARK.fg)),
        inner,
    );
    let cx = inner.x + (prompt.cursor as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((cx, inner.y));
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
        let km = &app.config.keymap;
        let hint_line = |chord: String, label: &str| {
            Line::from(vec![
                Span::styled(chord, Style::default().fg(DARK.accent).bold()),
                Span::styled(format!("  {label}"), Style::default().fg(DARK.dim)),
            ])
        };
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "No file open",
                Style::default().fg(DARK.fg).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Pick a file in the explorer and press Enter, or:",
                Style::default().fg(DARK.dim),
            )),
            Line::from(""),
            hint_line(km.new_file.to_string(), "new file"),
            hint_line(km.help.to_string(), "all keybindings"),
        ])
        .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    }

    let [tabbar, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    // Render tabs manually so each tab's screen x-range (and its ✕ button) is known for clicks.
    let active = app.editor.active;
    let infos: Vec<(usize, String, bool)> = app
        .editor
        .tabs
        .iter()
        .enumerate()
        .map(|(i, b)| (i, b.name.clone(), b.modified))
        .collect();
    let mut spans = Vec::new();
    let mut tab_hb = Vec::new();
    let mut close_hb = Vec::new();
    let mut x = tabbar.x;
    for (i, name, modified) in &infos {
        let marker = if *modified { "● " } else { "" };
        let body = format!(" {marker}{name} ");
        let bw = body.chars().count() as u16;
        let body_style = if *i == active {
            Style::default().fg(DARK.accent).bold()
        } else {
            Style::default().fg(DARK.dim)
        };
        spans.push(Span::styled(body, body_style));
        tab_hb.push((x, x + bw, *i));

        let close = "✕ ";
        let cw = close.chars().count() as u16;
        spans.push(Span::styled(close, Style::default().fg(DARK.dim)));
        close_hb.push((x + bw, x + bw + cw, *i));
        x += bw + cw;
    }
    app.editor.tab_hitboxes = tab_hb;
    app.editor.close_hitboxes = close_hb;
    app.editor.tabbar_row = tabbar.y;
    frame.render_widget(Paragraph::new(Line::from(spans)), tabbar);

    app.editor.viewport = content.height as usize;
    let (total, scroll) = app
        .editor
        .active_buffer()
        .map(|b| (b.lines.len(), b.scroll))
        .unwrap_or((0, 0));
    // Reserve the rightmost column for the scrollbar when it's shown.
    let show_sb = app.show_scrollbar && total > content.height as usize && content.width > 1;
    let text_area = if show_sb {
        Rect::new(content.x, content.y, content.width - 1, content.height)
    } else {
        content
    };
    app.editor.content_area = text_area;

    if let Some(buf) = app.editor.active_buffer() {
        let start = buf.scroll.min(total.saturating_sub(1));
        let end = (start + text_area.height as usize).min(total);
        let visible: Vec<Line> = buf.lines[start..end].to_vec();
        frame.render_widget(Paragraph::new(visible), text_area);
    }
    draw_selection(frame, app, text_area);

    // Show the cursor when the editor is focused and the cursor line is visible.
    if app.focus == Focus::Editor {
        if let Some(buf) = app.editor.active_buffer() {
            let (cl, cc) = buf.cursor;
            if cl >= buf.scroll && cl < buf.scroll + text_area.height as usize {
                let y = text_area.y + (cl - buf.scroll) as u16;
                let x = text_area.x + (cc as u16).min(text_area.width.saturating_sub(1));
                frame.set_cursor_position((x, y));
            }
        }
    }

    app.editor.scrollbar_col = if show_sb {
        Some(content.x + content.width - 1)
    } else {
        None
    };
    if show_sb {
        draw_scrollbar(frame, content, total, scroll);
    }
}

/// A custom vertical scrollbar in the rightmost column of `area`. The thumb is sized to the
/// visible fraction and sits flush at the bottom when scrolled to the end (unlike ratatui's
/// Scrollbar, which stops short).
fn draw_scrollbar(frame: &mut Frame, area: Rect, total: usize, scroll: usize) {
    let track = area.height as usize;
    if track == 0 || total == 0 {
        return;
    }
    let max_scroll = total.saturating_sub(track);
    let thumb = ((track * track) / total).clamp(1, track);
    let thumb_pos = if max_scroll == 0 {
        0
    } else {
        scroll * (track - thumb) / max_scroll
    };
    let col = area.x + area.width - 1;
    let buf = frame.buffer_mut();
    for i in 0..track {
        let on_thumb = i >= thumb_pos && i < thumb_pos + thumb;
        let (symbol, color) = if on_thumb {
            ("█", DARK.dim)
        } else {
            ("│", DARK.border)
        };
        if let Some(cell) = buf.cell_mut((col, area.y + i as u16)) {
            cell.set_symbol(symbol).set_fg(color);
        }
    }
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
        format!(" {} help · {} sidebar · {} quit ", km.help, km.toggle_sidebar, km.quit),
        bg,
    ));

    let [l, r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)])
            .areas(area);
    frame.render_widget(Paragraph::new(left).style(bg), l);
    frame.render_widget(Paragraph::new(right).style(bg).alignment(Alignment::Right), r);
}
