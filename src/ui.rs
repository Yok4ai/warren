//! Top-level rendering: sidebar (explorer) + editor area (tabs + content) + statusline.
//! Phase 5 replaces the fixed sidebar|editor split with the recursive layout tree.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::theme::DARK;

const SIDEBAR_WIDTH: u16 = 32;

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

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color(focused)))
        .title(Span::styled(
            " Explorer ",
            Style::default().fg(DARK.accent).bold(),
        ));

    let items: Vec<ListItem> = app
        .tree
        .rows
        .iter()
        .map(|row| {
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
            let text_style = if row.is_dir {
                Style::default().fg(DARK.accent)
            } else {
                Style::default().fg(DARK.fg)
            };
            ListItem::new(Line::from(Span::styled(
                format!("{indent}{icon}{}", row.name),
                text_style,
            )))
        })
        .collect();

    let highlight = if focused {
        Style::default().bg(DARK.accent).fg(Color::Black)
    } else {
        Style::default().bg(DARK.status_bg)
    };
    let list = List::new(items).block(block).highlight_style(highlight);
    let mut state = ListState::default();
    state.select(Some(app.tree.selected));
    frame.render_stateful_widget(list, area, &mut state);
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

    let titles: Vec<Line> = app
        .editor
        .tabs
        .iter()
        .map(|b| Line::from(format!(" {} ", b.name)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.editor.active)
        .style(Style::default().fg(DARK.dim))
        .highlight_style(Style::default().fg(DARK.accent).bold())
        .divider("");
    frame.render_widget(tabs, tabbar);

    app.editor.viewport = content.height as usize;
    if let Some(buf) = app.editor.active_buffer() {
        let start = buf.scroll.min(buf.line_count().saturating_sub(1));
        let end = (start + content.height as usize).min(buf.line_count());
        let visible: Vec<Line> = buf.lines[start..end].to_vec();
        frame.render_widget(Paragraph::new(visible), content);
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
