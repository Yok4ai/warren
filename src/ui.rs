//! Top-level rendering: sidebar (explorer) + editor area (tabs + content) + statusline.
//! Phase 5 replaces the fixed sidebar|editor split with the recursive layout tree.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::palette::Palette;
use crate::prompt::Prompt;
use ratatui::Frame;

use ratatui_image::StatefulImage;

use crate::app::{App, Focus, ScmItem, SidebarMode};
use crate::editor::DiffKind;
use crate::find::Search;
use crate::theme;

/// The active theme (switchable at runtime).
fn dark() -> &'static theme::Theme {
    theme::current()
}

/// Width of a collapsed pane's rail, in columns.
const RAIL_W: u16 = 3;

/// Draw a full-width horizontal rule across `area`'s first row (pane separator).
fn hrule(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for x in area.x..area.x + area.width {
        if let Some(c) = buf.cell_mut((x, area.y)) {
            c.set_symbol("─");
            c.set_style(Style::default().fg(dark().border));
        }
    }
}

/// Draw a single vertical divider down `col` across the body rows `[y, y+h)`.
fn vrule(frame: &mut Frame, col: u16, y: u16, h: u16) {
    let buf = frame.buffer_mut();
    for r in y..y + h {
        if let Some(c) = buf.cell_mut((col, r)) {
            c.set_symbol("│");
            c.set_style(Style::default().fg(dark().border));
        }
    }
}

/// The global top app bar: the warren mark, a breadcrumb of the open file, and window controls
/// (minimize the focused pane · zen · quit). Controls' hitboxes are stashed for mouse handling.
fn draw_app_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut spans = vec![
        Span::styled("▎", Style::default().fg(dark().accent)),
        Span::styled("warren", Style::default().fg(dark().accent).bold()),
        Span::raw("   "),
    ];
    // Breadcrumb of the active file, workspace-relative: dir › dir › file (file brighter).
    if let Some(b) = app.editor.active_buffer() {
        if b.path.is_absolute() {
            let rel = b.path.strip_prefix(&app.workspace).unwrap_or(&b.path);
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let n = parts.len();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" › ", Style::default().fg(dark().dim)));
                }
                let style = if i + 1 == n {
                    Style::default().fg(dark().fg)
                } else {
                    Style::default().fg(dark().dim)
                };
                spans.push(Span::styled(part.clone(), style));
            }
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw a collapsed pane as a thin rail: an accent expand-button cap at the top and the pane name
/// running vertically below it. The whole rail highlights on hover; clicking it restores the pane.
fn draw_rail(frame: &mut Frame, area: Rect, label: &str, chevron: char, hover: (u16, u16)) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hovered = (area.x..area.x + area.width).contains(&hover.0)
        && (area.y..area.y + area.height).contains(&hover.1);
    let gs = chevron.to_string();
    let buf = frame.buffer_mut();
    // Accent button cap at the top.
    if let Some(cell) = buf.cell_mut((area.x, area.y)) {
        cell.set_symbol(&gs);
        cell.set_style(Style::default().bg(dark().accent).fg(Color::Black).bold());
    }
    let label_fg = if hovered { dark().accent } else { dark().dim };
    for (i, ch) in label.chars().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol(&ch.to_string());
            cell.set_style(Style::default().fg(label_fg));
        }
    }
}

/// Draw a flat pane label header on the first row of `area`: the title (accent+bold when focused,
/// dim otherwise) on the left and a collapse chevron on the right. Returns the content area below it
/// and the chevron's hitbox cell (click to collapse the pane).
fn pane_label(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    collapse: char,
    hover: (u16, u16),
) -> (Rect, Option<(u16, u16)>) {
    if area.height == 0 {
        return (area, None);
    }
    let row = Rect::new(area.x, area.y, area.width, 1);
    let style = if focused {
        Style::default().fg(dark().accent).bold()
    } else {
        Style::default().fg(dark().dim)
    };
    frame.render_widget(Paragraph::new(Line::from(format!(" {title}"))).style(style), row);
    let content = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    (content, collapse_button(frame, area, collapse, hover))
}

/// Draw a 3-cell pill button with `glyph` centered, at columns `[x0, x0+2]` of row `y`. Highlights
/// (accent fill) on hover, otherwise a subtle raised chip. Returns the center cell.
fn pill_button(frame: &mut Frame, x0: u16, y: u16, glyph: char, hover: (u16, u16)) -> (u16, u16) {
    let center = x0 + 1;
    let hovered = hover.1 == y && (x0..x0 + 3).contains(&hover.0);
    let (bg, fg) = if hovered {
        (dark().accent, Color::Black)
    } else {
        (dark().border, dark().accent)
    };
    let gs = glyph.to_string();
    let buf = frame.buffer_mut();
    for cx in x0..x0 + 3 {
        if let Some(c) = buf.cell_mut((cx, y)) {
            let mut s = Style::default().bg(bg);
            if cx == center {
                c.set_symbol(&gs);
                s = s.fg(fg).bold();
            } else {
                c.set_symbol(" ");
            }
            c.set_style(s);
        }
    }
    (center, y)
}

/// The collapse button: a pill at the right of a header `area`'s first row (one trailing margin).
fn collapse_button(frame: &mut Frame, area: Rect, glyph: char, hover: (u16, u16)) -> Option<(u16, u16)> {
    if area.width < 5 {
        return None;
    }
    Some(pill_button(frame, area.x + area.width - 4, area.y, glyph, hover))
}

/// Lay out the body: sidebar | editor | terminal as flat columns separated by vertical rules, each
/// pane respecting its mode (full / collapsed to a rail / hidden). Records per-frame geometry.
fn layout_body(frame: &mut Frame, app: &mut App, body: Rect) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Hidden,
        Rail,
        Full,
    }
    let mode = |visible: bool, collapsed: bool| {
        if !visible {
            Mode::Hidden
        } else if collapsed {
            Mode::Rail
        } else {
            Mode::Full
        }
    };
    let sb = mode(app.sidebar_visible, app.sidebar_collapsed);
    let ed = mode(app.editor_visible, app.editor_collapsed);
    // The terminal shows as a rail when collapsed (even with no shell yet); only a full, non-empty
    // panel renders content. An expanded-but-empty panel is treated as hidden (shouldn't occur —
    // expanding spawns a shell).
    let pn = if !app.panel.visible || (!app.panel_collapsed && app.panel.is_empty()) {
        Mode::Hidden
    } else if app.panel_collapsed {
        Mode::Rail
    } else {
        Mode::Full
    };

    #[derive(Clone, Copy, PartialEq)]
    enum Seg {
        Sidebar,
        SidebarRail,
        Editor,
        EditorRail,
        Panel,
        PanelRail,
        DivSidebar,
        DivTerm,
        Div,
        Hint,
    }

    let sb_w = match sb {
        Mode::Full => app.sidebar_width,
        Mode::Rail => RAIL_W,
        Mode::Hidden => 0,
    };
    // Terminal width when both editor and panel are full (term_ratio % of the editor+panel region).
    let n_div = (sb != Mode::Hidden) as u16 + 1;
    let region = body.width.saturating_sub(sb_w + n_div);
    let term_w = if region >= 14 {
        (((region as u32 * app.term_ratio as u32) / 100) as u16).clamp(6, region - 6)
    } else {
        region / 2
    };

    // Build the panes left→right WITHOUT dividers. Order keeps the explorer leftmost and the
    // terminal rightmost; a collapsed editor tucks its rail next to the explorer, and an empty
    // middle gets the hint. The flex (Min(0)) pane absorbs the slack.
    let mut panes: Vec<(Constraint, Seg)> = Vec::new();
    match sb {
        Mode::Full => panes.push((Constraint::Length(sb_w), Seg::Sidebar)),
        Mode::Rail => panes.push((Constraint::Length(RAIL_W), Seg::SidebarRail)),
        Mode::Hidden => {}
    }
    if ed == Mode::Rail {
        panes.push((Constraint::Length(RAIL_W), Seg::EditorRail));
    }
    if ed == Mode::Full {
        panes.push((Constraint::Min(0), Seg::Editor));
        match pn {
            Mode::Full => panes.push((Constraint::Length(term_w), Seg::Panel)),
            Mode::Rail => panes.push((Constraint::Length(RAIL_W), Seg::PanelRail)),
            Mode::Hidden => {}
        }
    } else {
        // Editor not full: the terminal (if full) flexes to fill; otherwise a hint fills the middle.
        match pn {
            Mode::Full => panes.push((Constraint::Min(0), Seg::Panel)),
            Mode::Rail => {
                panes.push((Constraint::Min(0), Seg::Hint));
                panes.push((Constraint::Length(RAIL_W), Seg::PanelRail));
            }
            Mode::Hidden => panes.push((Constraint::Min(0), Seg::Hint)),
        }
    }

    // Interleave a 1-col divider between adjacent panes. The divider right of a full sidebar resizes
    // it; the one between a full editor and full terminal resizes the split; others are decorative.
    let mut segs: Vec<(Constraint, Seg)> = Vec::new();
    for (i, (c, s)) in panes.iter().enumerate() {
        if i > 0 {
            let prev = panes[i - 1].1;
            let kind = if prev == Seg::Sidebar {
                Seg::DivSidebar
            } else if prev == Seg::Editor && *s == Seg::Panel {
                Seg::DivTerm
            } else {
                Seg::Div
            };
            segs.push((Constraint::Length(1), kind));
        }
        segs.push((*c, *s));
    }

    let constraints: Vec<Constraint> = segs.iter().map(|(c, _)| *c).collect();
    let rects = Layout::horizontal(constraints).split(body);
    for (rect, (_, seg)) in rects.iter().zip(segs.iter()) {
        let r = *rect;
        match seg {
            Seg::Sidebar => draw_sidebar(frame, app, r),
            Seg::SidebarRail => {
                draw_rail(frame, r, "EXPLORER", '›', app.hover);
                app.sidebar_rail = Some(r);
            }
            Seg::Editor => {
                app.editor_area = r;
                draw_editor(frame, app, r);
            }
            Seg::EditorRail => {
                draw_rail(frame, r, "EDITOR", '›', app.hover);
                app.editor_rail = Some(r);
            }
            Seg::Panel => {
                app.terminal_area = r;
                draw_panel(frame, app, r);
            }
            Seg::PanelRail => {
                draw_rail(frame, r, "TERMINAL", '‹', app.hover);
                app.panel_rail = Some(r);
            }
            Seg::DivSidebar => {
                vrule(frame, r.x, r.y, r.height);
                app.sidebar_divider_col = Some(r.x);
            }
            Seg::DivTerm => {
                vrule(frame, r.x, r.y, r.height);
                app.term_divider_col = Some(r.x);
            }
            Seg::Div => vrule(frame, r.x, r.y, r.height),
            Seg::Hint => draw_all_hidden(frame, app, r),
        }
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.term_width = frame.area().width;
    app.editor_hbar = None;
    let [appbar, sep1, body, sep2, status] = Layout::vertical([
        Constraint::Length(1), // global app bar
        Constraint::Length(1), // rule
        Constraint::Min(0),    // panes
        Constraint::Length(1), // rule
        Constraint::Length(1), // status bar
    ])
    .areas(frame.area());

    // Reset per-frame chrome geometry.
    app.sidebar_rail = None;
    app.sidebar_body = None;
    app.sidebar_collapse_hit = None;
    app.explorer_new_hit = None;
    app.panel_collapse_hit = None;
    app.editor_collapse_hit = None;
    app.sidebar_divider_col = None;
    app.panel_rail = None;
    app.editor_rail = None;
    app.term_divider_col = None;
    app.editor_area = Rect::default();
    app.terminal_area = Rect::default();

    draw_app_bar(frame, app, appbar);
    hrule(frame, sep1);
    layout_body(frame, app, body);
    hrule(frame, sep2);
    draw_status(frame, app, status);

    if let Some(prompt) = app.prompt.as_ref() {
        draw_prompt(frame, prompt, frame.area());
    }
    if let Some(p) = app.palette.as_ref() {
        draw_palette(frame, p, frame.area());
    }
    if let Some(s) = app.search.as_ref() {
        let area = if app.editor_area.width > 0 {
            app.editor_area
        } else {
            frame.area()
        };
        draw_find_bar(frame, s, area);
    }
    if let Some(idx) = app.close_confirm {
        if let Some(name) = app.editor.tabs.get(idx).map(|b| b.name.as_str()) {
            draw_confirm(frame, name, frame.area());
        }
    }
    if let Some(path) = app.delete_confirm.clone() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let kind = if path.is_dir() { "folder" } else { "file" };
        draw_delete_confirm(frame, &name, kind, frame.area());
    }
    if app.show_help {
        draw_help(frame, app, frame.area());
    }
    if app.context_menu.is_some() {
        draw_context_menu(frame, app, frame.area());
    }
    if app.dragging {
        draw_drag_label(frame, app, frame.area());
    }

    // Solid background: as the last step, fill every still-transparent cell with the theme bg.
    // This catches the terminal pane (tui-term writes Reset backgrounds) and the overlays
    // (Clear resets to default), while leaving cells with real colors untouched.
    if app.solid_bg {
        let area = frame.area();
        let bg = dark().bg;
        let buf = frame.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    if cell.bg == Color::Reset {
                        cell.set_bg(bg);
                    }
                }
            }
        }
    }
}

/// A small floating tag that follows the cursor while dragging a file from the explorer.
fn draw_drag_label(frame: &mut Frame, app: &App, area: Rect) {
    let Some(path) = app.drag_source.as_ref() else {
        return;
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = format!(" {name} ");
    let w = (label.chars().count() as u16).min(area.width);
    let x = (app.drag_pos.0 + 1).min(area.width.saturating_sub(w));
    let y = app.drag_pos.1.min(area.height.saturating_sub(1));
    let rect = Rect::new(x, y, w, 1);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().bg(dark().accent).fg(Color::Black).bold(),
        ))),
        rect,
    );
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
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent))
        .title(Span::styled(
            " Keybindings — Esc to close ",
            Style::default().fg(dark().accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let lines: Vec<Line> = bindings
        .iter()
        .map(|(label, chord)| {
            Line::from(vec![
                Span::styled(format!(" {:<14}", chord.to_string()), Style::default().fg(dark().accent)),
                Span::styled((*label).to_string(), Style::default().fg(dark().fg)),
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
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent))
        .title(Span::styled(
            " Unsaved changes ",
            Style::default().fg(dark().accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let key = |k: &'static str| Span::styled(k, Style::default().fg(dark().accent).bold());
    let dim = |t: &str| Span::styled(t.to_string(), Style::default().fg(dark().dim));
    let lines = vec![
        Line::from(Span::styled(
            format!("{name} has unsaved changes."),
            Style::default().fg(dark().fg),
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

/// A centered confirm dialog for deleting a file/folder.
fn draw_delete_confirm(frame: &mut Frame, name: &str, kind: &str, area: Rect) {
    let w = area.width.saturating_sub(8).clamp(34, 64);
    let h = 5;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 3;
    let rect = Rect::new(x, y, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().diff_del_fg))
        .title(Span::styled(
            " Delete ",
            Style::default().fg(dark().diff_del_fg).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let key = |k: &'static str| Span::styled(k, Style::default().fg(dark().accent).bold());
    let dim = |t: &str| Span::styled(t.to_string(), Style::default().fg(dark().dim));
    let lines = vec![
        Line::from(Span::styled(
            format!("Delete {kind} \"{name}\"? This can't be undone."),
            Style::default().fg(dark().fg),
        )),
        Line::from(""),
        Line::from(vec![key("[Y]"), dim("es   "), key("[N]"), dim("o")]),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// The explorer right-click context menu: a small popup list, clamped to the screen. Stores its
/// rendered rect on the menu for click mapping.
fn draw_context_menu(frame: &mut Frame, app: &mut App, screen: Rect) {
    let Some(menu) = app.context_menu.as_mut() else {
        return;
    };
    let label_w = menu.items.iter().map(|i| i.label.len()).max().unwrap_or(4) as u16;
    let w = (label_w + 4).min(screen.width.max(6));
    let h = menu.items.len() as u16 + 2;
    // Anchor at the cursor, clamped so the menu stays fully on-screen.
    let x = menu.x.min(screen.right().saturating_sub(w));
    let y = menu.y.min(screen.bottom().saturating_sub(h));
    let rect = Rect::new(x, y, w, h);
    menu.rect = rect;

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if !item.enabled {
                Style::default().fg(dark().dim)
            } else if i == menu.selected {
                Style::default().bg(dark().accent).fg(Color::Black).bold()
            } else {
                Style::default().fg(dark().fg)
            };
            let iw = inner.width as usize;
            Line::from(Span::styled(format!(" {:<iw$}", item.label), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The command palette: an input line plus a scrolling, fuzzy-filtered results list.
fn draw_palette(frame: &mut Frame, p: &Palette, area: Rect) {
    let w = (area.width * 3 / 5).clamp(40, 90).min(area.width.saturating_sub(2));
    let h = 16.min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 4;
    let rect = Rect::new(x, y, w, h);

    frame.render_widget(Clear, rect);
    let title = if p.command_mode() {
        " Commands "
    } else {
        " Open File — type, or > for commands "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent))
        .title(Span::styled(title, Style::default().fg(dark().accent).bold()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let [input_row, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    if p.input.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "search files…",
                Style::default().fg(dark().dim),
            )),
            input_row,
        );
    } else {
        frame.render_widget(
            Paragraph::new(Span::styled(p.input.as_str(), Style::default().fg(dark().fg))),
            input_row,
        );
    }
    let cx = input_row.x + (p.cursor as u16).min(input_row.width.saturating_sub(1));
    frame.set_cursor_position((cx, input_row.y));

    let height = list.height as usize;
    let offset = if p.selected >= height {
        p.selected + 1 - height
    } else {
        0
    };
    let width = list.width as usize;
    let lines: Vec<Line> = p
        .results
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, s)| {
            if i == p.selected {
                Line::from(Span::styled(
                    format!("{s:<width$}"),
                    Style::default().bg(dark().accent).fg(Color::Black),
                ))
            } else {
                Line::from(Span::styled(s.clone(), Style::default().fg(dark().fg)))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
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
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent))
        .title(Span::styled(
            format!(" {} ", prompt.title),
            Style::default().fg(dark().accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(prompt.input.as_str()).style(Style::default().fg(dark().fg)),
        inner,
    );
    let cx = inner.x + (prompt.cursor as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((cx, inner.y));
}

fn border_color(focused: bool) -> Color {
    if focused {
        dark().accent
    } else {
        dark().border
    }
}

/// Conventional git status colors (theme-independent, like a real VCS gutter).
const GIT_GREEN: Color = Color::Rgb(0x9e, 0xce, 0x6a);
const GIT_YELLOW: Color = Color::Rgb(0xe0, 0xaf, 0x68);
const GIT_RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);

/// Map a working-tree status code to a one-char sign + color for explorer rows.
fn git_mark(code: char) -> (char, Color) {
    match code {
        'U' => ('?', GIT_GREEN), // untracked
        'A' => ('+', GIT_GREEN), // added/staged-new
        'D' => ('-', GIT_RED),   // deleted
        'C' => ('!', GIT_RED),   // conflict
        _ => ('~', GIT_YELLOW),  // modified/renamed/typechange
    }
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    app.sidebar_sb = None;
    let focused = app.focus == Focus::Sidebar;
    let title = match app.sidebar_mode {
        SidebarMode::Explorer => "EXPLORER".to_string(),
        SidebarMode::SourceControl if app.git_branch.is_empty() => "SOURCE CONTROL".to_string(),
        SidebarMode::SourceControl => format!("SOURCE CONTROL — {}", app.git_branch),
    };
    let (content, hit) = pane_label(frame, area, &title, focused, '‹', app.hover);
    app.sidebar_collapse_hit = hit;
    // Explorer header "+" button (New File / New Folder), a pill just left of the collapse button.
    if app.sidebar_mode == SidebarMode::Explorer && area.width >= 9 {
        let x0 = (area.x + area.width).saturating_sub(8); // 3-cell pill + 1 gap before collapse
        app.explorer_new_hit = Some(pill_button(frame, x0, area.y, '+', app.hover));
    }
    match app.sidebar_mode {
        SidebarMode::Explorer => draw_explorer(frame, app, content),
        SidebarMode::SourceControl => draw_scm(frame, app, content),
    }
}

/// Source-control view: a CHANGES list and a GRAPH (commit) list, with one selection across both.
fn draw_scm(frame: &mut Frame, app: &mut App, area: Rect) {
    let inner = area;

    if !app.has_git() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "not a git repository",
                Style::default().fg(dark().dim),
            )),
            inner,
        );
        return;
    }

    let width = inner.width as usize;
    let n_changes = app.git_changes.len();
    let mut lines: Vec<Line> = Vec::new();
    let mut item_line: Vec<(usize, usize)> = Vec::new();
    let mut graph_emitted = false;

    lines.push(Line::from(Span::styled(
        format!("CHANGES ({n_changes})"),
        Style::default().fg(dark().dim).bold(),
    )));
    for (idx, item) in app.scm_items.iter().enumerate() {
        let sel = idx == app.scm_selected;
        match item {
            ScmItem::Change(i) => {
                item_line.push((lines.len(), idx));
                lines.push(scm_change_line(&app.git_changes[*i], sel, width));
            }
            ScmItem::Commit(j) => {
                if !graph_emitted {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(Span::styled(
                        "GRAPH",
                        Style::default().fg(dark().dim).bold(),
                    )));
                    graph_emitted = true;
                }
                let c = &app.git_commits[*j];
                let twisty = if app.git_expanded.contains(&c.id) {
                    "▾"
                } else {
                    "▸"
                };
                let text = format!("{twisty} {} · {} · {}  {}", c.short, c.author, c.when, c.summary);
                let text: String = text.chars().take(width).collect();
                let style = if sel {
                    Style::default().bg(dark().accent).fg(Color::Black)
                } else {
                    Style::default().fg(dark().fg)
                };
                item_line.push((lines.len(), idx));
                lines.push(Line::from(Span::styled(
                    if sel { format!("{text:<width$}") } else { text },
                    style,
                )));
            }
            ScmItem::CommitFile { code, path, .. } => {
                let text = format!("      {code} {path}");
                let text: String = text.chars().take(width).collect();
                let style = if sel {
                    Style::default().bg(dark().accent).fg(Color::Black)
                } else {
                    Style::default().fg(dark().dim)
                };
                item_line.push((lines.len(), idx));
                lines.push(Line::from(Span::styled(
                    if sel { format!("{text:<width$}") } else { text },
                    style,
                )));
            }
        }
    }

    // Reserve a column for a scrollbar when the content overflows.
    let total = lines.len();
    let show_sb = total > inner.height as usize && inner.width > 1;
    let content = if show_sb {
        Rect::new(inner.x, inner.y, inner.width - 1, inner.height)
    } else {
        inner
    };

    // Independent scroll offset (selection only nudges it via scm_ensure_visible on key nav).
    let h = content.height as usize;
    app.scm_viewport = h;
    app.scm_total_lines = total;
    let mut item_lines = vec![0usize; app.scm_items.len()];
    for (line, item) in &item_line {
        if *item < item_lines.len() {
            item_lines[*item] = *line;
        }
    }
    app.scm_item_lines = item_lines;
    app.scm_scroll = app.scm_scroll.min(total.saturating_sub(h));
    let off = app.scm_scroll;

    app.scm_rows = item_line
        .iter()
        .filter(|(li, _)| *li >= off && *li < off + h)
        .map(|(li, item)| (content.y + (li - off) as u16, *item))
        .collect();

    let visible: Vec<Line> = lines.into_iter().skip(off).take(h).collect();
    frame.render_widget(Paragraph::new(visible), content);
    if show_sb {
        draw_scrollbar(frame, inner, total, off);
        app.sidebar_sb = Some((inner.x + inner.width - 1, inner.y, inner.height, total));
    }
}

fn scm_change_line(ch: &crate::git::Change, selected: bool, width: usize) -> Line<'static> {
    let code_color = match ch.code {
        'A' | 'U' => Color::Green,
        'D' | 'C' => Color::Red,
        'R' => Color::Cyan,
        _ => Color::Yellow,
    };
    let dot = if ch.staged { "●" } else { " " };
    if selected {
        let text = format!("{dot}{} {}", ch.code, ch.path);
        let text: String = text.chars().take(width).collect();
        Line::from(Span::styled(
            format!("{text:<width$}"),
            Style::default().bg(dark().accent).fg(Color::Black),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                dot.to_string(),
                Style::default().fg(if ch.staged { Color::Green } else { dark().dim }),
            ),
            Span::styled(format!("{} ", ch.code), Style::default().fg(code_color)),
            Span::styled(ch.path.clone(), Style::default().fg(dark().fg)),
        ])
    }
}

fn draw_explorer(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let inner = area;

    // Reserve a column for a scrollbar when the tree overflows.
    let total = app.tree.rows.len();
    let show_sb = total > inner.height as usize && inner.width > 1;
    let content = if show_sb {
        Rect::new(inner.x, inner.y, inner.width - 1, inner.height)
    } else {
        inner
    };
    app.sidebar_body = Some(content);

    let height = content.height as usize;
    let width = content.width as usize;
    // Independent scroll offset (clamped); keyboard nav adjusts it via ensure_visible.
    app.tree.viewport = height;
    let max = total.saturating_sub(height);
    app.tree.scroll = app.tree.scroll.min(max);
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
            let (glyph, icol) =
                crate::icons::file_icon(&row.name, row.is_dir, row.expanded, app.config.icons);
            // Git status: a file gets its own status color + sign; a directory containing changes
            // gets a muted dot. The sign is right-aligned at the pane edge (neo-tree style).
            let mark = if row.is_dir {
                app.git_dirty_dirs
                    .contains(&row.path)
                    .then_some(('\u{2022}', dark().dim)) // • dot
            } else {
                app.git_marks.get(&row.path).map(|&c| git_mark(c))
            };
            if i == app.tree.selected {
                // Pad to full width so the selection bar spans the pane.
                let style = if focused {
                    Style::default().bg(dark().accent).fg(Color::Black)
                } else {
                    Style::default().bg(dark().status_bg).fg(dark().fg)
                };
                let sign = mark.map(|(s, _)| s);
                let label = format!("{indent}{glyph} {}", row.name);
                let used = label.chars().count();
                let line = match sign {
                    Some(s) if used + 2 <= width => {
                        format!("{label}{:pad$}{s}", "", pad = width - used - 1)
                    }
                    _ => format!("{label:<width$}"),
                };
                Line::from(Span::styled(line, style))
            } else {
                // A changed file's name takes the status color; otherwise dir=accent, file=fg.
                let name_style = match (row.is_dir, mark) {
                    (false, Some((_, c))) => Style::default().fg(c),
                    (true, _) => Style::default().fg(dark().accent).bold(),
                    _ => Style::default().fg(dark().fg),
                };
                let mut spans = vec![
                    Span::raw(indent.clone()),
                    Span::styled(format!("{glyph} "), Style::default().fg(icol)),
                    Span::styled(row.name.clone(), name_style),
                ];
                if let Some((sign, col)) = mark {
                    let used = indent.chars().count() + 2 + row.name.chars().count();
                    if used + 2 <= width {
                        spans.push(Span::raw(" ".repeat(width - used - 1)));
                        spans.push(Span::styled(sign.to_string(), Style::default().fg(col)));
                    }
                }
                Line::from(spans)
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), content);
    if show_sb {
        draw_scrollbar(frame, inner, total, scroll);
        app.sidebar_sb = Some((inner.x + inner.width - 1, inner.y, inner.height, total));
    }
}

fn draw_editor(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Editor;
    let inner = area;

    if app.editor.tabs.is_empty() {
        app.editor.viewport = 0;
        let km = &app.config.keymap;
        // The welcome screen has no tab bar, so place the collapse button at the top-right itself.
        app.editor_collapse_hit = collapse_button(frame, inner, '‹', app.hover);

        // "WARREN" in opencode's pixel font. `E` and `N` are the genuine opencode glyphs (extracted
        // from their open-source wordmark SVG, a 4×5 grid); `W`/`A`/`R` are drawn to match the same
        // grid + stroke weight. `#` = filled, `.` = blank. Rendered at 2× width so the pixels read
        // square (terminal cells are ~1:2), dropping to 1× then a plain word as the pane narrows.
        const W: [&str; 5] = ["#...#", "#...#", "#.#.#", "#.#.#", ".#.#."];
        const A: [&str; 5] = ["####", "#..#", "####", "#..#", "#..#"];
        const R: [&str; 5] = ["####", "#..#", "####", "#.#.", "#..#"];
        const E: [&str; 5] = ["####", "#..#", "####", "#...", "####"];
        const N: [&str; 5] = ["###.", "#..#", "#..#", "#..#", "#..#"];
        let letters = [W, A, R, R, E, N];
        let art: Vec<String> = (0..5)
            .map(|row| letters.iter().map(|l| l[row]).collect::<Vec<_>>().join(" "))
            .collect();
        let cells = art.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        // Largest integer scale that fits (2× gives square pixels), else 1×.
        let scale: u16 = if inner.width >= cells * 2 { 2 } else { 1 };
        let banner_w = cells * scale;

        let [_, ba, meta, hbot] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(art.len() as u16),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .areas(inner);

        // Center the banner as a fixed-width block so the art stays aligned (fall back to a plain
        // word if the pane is too narrow even at 1×).
        if inner.width >= banner_w {
            let [_, bm, _] = Layout::horizontal([
                Constraint::Min(0),
                Constraint::Length(banner_w),
                Constraint::Min(0),
            ])
            .areas(ba);
            // Subtle left-to-right neutral-grey gradient ending at the wordmark's light grey
            // (#e0e0e0, from WARREN.svg) — theme-independent so it reads the same everywhere.
            let total = banner_w.max(1) as f32 - 1.0;
            let banner: Vec<Line> = art
                .iter()
                .map(|l| {
                    let mut spans = Vec::with_capacity(banner_w as usize);
                    let mut col = 0u16;
                    for ch in l.chars() {
                        let glyph = if ch == '#' { "\u{2588}" } else { " " };
                        for _ in 0..scale {
                            let f = (col as f32 / total).clamp(0.0, 1.0);
                            // 0x9a (medium grey) → 0xe0 (the wordmark grey).
                            let v = (0x9a as f32 + (0xe0 - 0x9a) as f32 * f).round() as u8;
                            let style = Style::default().fg(Color::Rgb(v, v, v)).bold();
                            spans.push(Span::styled(glyph.to_string(), style));
                            col += 1;
                        }
                    }
                    Line::from(spans)
                })
                .collect();
            frame.render_widget(Paragraph::new(banner), bm);
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "warren",
                    Style::default().fg(dark().accent).bold(),
                )))
                .alignment(Alignment::Center),
                ba,
            );
        }

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(dark().dim),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Pick a file in the explorer and press Enter, or:",
                    Style::default().fg(dark().fg),
                )),
            ])
            .alignment(Alignment::Center),
            meta,
        );

        // LazyVim-style menu: a filetype/action glyph (Nerd Font only), a right-aligned chord
        // column, a bullet, then a bright label.
        let nerd = app.config.icons == crate::icons::IconStyle::Nerd;
        let hint = |icon: &str, chord: String, label: &str| {
            let ic = if nerd { format!("{icon}  ") } else { String::new() };
            Line::from(vec![
                Span::styled(ic, Style::default().fg(dark().accent)),
                Span::styled(format!("{chord:>6}"), Style::default().fg(dark().accent).bold()),
                Span::styled(format!("  \u{2022} {label}"), Style::default().fg(Color::White)),
            ])
        };
        let hints = Paragraph::new(vec![
            hint("\u{f002}", km.command_palette.to_string(), "find file / commands"),
            hint("\u{f15b}", km.new_file.to_string(), "new file"),
            hint("\u{f489}", km.new_terminal.to_string(), "new terminal"),
            hint("\u{f00d}", km.close_tab.to_string(), "close tab / terminal"),
            hint("\u{e795}", km.toggle_panel.to_string(), "toggle terminal panel"),
            hint("\u{f07b}", km.toggle_sidebar.to_string(), "toggle sidebar"),
            hint("\u{f044}", km.toggle_editor.to_string(), "toggle editor"),
            hint("\u{e702}", km.toggle_scm.to_string(), "source control"),
            hint("\u{f11c}", km.help.to_string(), "all keybindings"),
        ]);

        // Center the aligned hint block as a fixed-width column.
        let [_, mid, _] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(40),
            Constraint::Min(0),
        ])
        .areas(hbot);
        frame.render_widget(hints, mid);
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
        let (glyph, gcol) = crate::icons::file_icon(name, false, false, app.config.icons);
        let marker = if *modified { "\u{25cf} " } else { "" };
        // Leading filetype icon (empty in `none` mode), kept in its brand color like bufferline.
        let icon_seg = if glyph.is_empty() {
            String::new()
        } else {
            format!(" {glyph}")
        };
        let body = format!(" {marker}{name} ");
        let iw = icon_seg.chars().count() as u16;
        let bw = body.chars().count() as u16;
        let span_w = iw + bw;
        let body_style = if *i == active {
            Style::default().fg(dark().accent).bold()
        } else {
            Style::default().fg(dark().dim)
        };
        if !icon_seg.is_empty() {
            spans.push(Span::styled(icon_seg, Style::default().fg(gcol)));
        }
        spans.push(Span::styled(body, body_style));
        tab_hb.push((x, x + span_w, *i));

        let close = "✕ ";
        let cw = close.chars().count() as u16;
        spans.push(Span::styled(close, Style::default().fg(dark().dim)));
        close_hb.push((x + span_w, x + span_w + cw, *i));
        x += span_w + cw;
    }
    app.editor.tab_hitboxes = tab_hb;
    app.editor.close_hitboxes = close_hb;
    app.editor.tabbar_row = tabbar.y;
    frame.render_widget(Paragraph::new(Line::from(spans)), tabbar);
    // Collapse button at the right of the tab bar (collapse the editor to a rail).
    app.editor_collapse_hit = collapse_button(frame, tabbar, '‹', app.hover);

    // Image buffers render via the terminal graphics protocol (kitty/sixel/iTerm2/half-blocks).
    if app.editor.active_buffer().map(|b| b.image.is_some()).unwrap_or(false) {
        app.editor.viewport = content.height as usize;
        if let Some(proto) = app.editor.active_buffer_mut().and_then(|b| b.image.as_mut()) {
            frame.render_stateful_widget(StatefulImage::default(), content, proto);
        }
        return;
    }

    // Markdown preview replaces the source rendering entirely (read-only, no gutter/cursor).
    if app
        .editor
        .active_buffer()
        .map(|b| b.preview && b.is_markdown())
        .unwrap_or(false)
    {
        draw_markdown_preview(frame, app, content);
        return;
    }

    app.editor.viewport = content.height as usize;
    let (total, scroll, cursor_line) = app
        .editor
        .active_buffer()
        .map(|b| (b.lines.len(), b.scroll, b.cursor.0))
        .unwrap_or((0, 0, 0));

    // Line-number gutter on the left. A full-file inline diff uses a dual gutter (old │ new);
    // a normal buffer uses a single column of line numbers.
    let diff_widths = app.editor.active_buffer().and_then(|b| {
        b.diff_rows.as_ref().map(|rows| {
            let max_old = rows.iter().filter_map(|r| r.old).max().unwrap_or(0);
            let max_new = rows.iter().filter_map(|r| r.new).max().unwrap_or(0);
            (
                max_old.max(1).to_string().len() as u16,
                max_new.max(1).to_string().len() as u16,
            )
        })
    });
    let digits = total.max(1).to_string().len() as u16;
    let gutter_w = match diff_widths {
        // "<old> <new> " plus a leading space.
        Some((ow, nw)) => ow + 1 + nw + 2,
        None => digits + 1,
    };
    let [gutter, body] =
        Layout::horizontal([Constraint::Length(gutter_w), Constraint::Min(0)]).areas(content);

    // Reserve the rightmost column (vertical scrollbar) and bottom row (horizontal scrollbar).
    let show_sb = app.show_scrollbar && total > body.height as usize && body.width > 1;
    let text_w = body.width.saturating_sub(u16::from(show_sb));
    let hscroll = app.editor.active_buffer().map(|b| b.hscroll).unwrap_or(0);
    // Widest line currently in view determines whether/how far we can scroll horizontally.
    let max_w = app
        .editor
        .active_buffer()
        .map(|b| {
            let s = b.scroll.min(total.saturating_sub(1));
            let e = (s + body.height as usize).min(total);
            (s..e)
                .map(|i| b.line_text(i).chars().count())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let show_hbar = app.show_scrollbar && max_w > text_w as usize && body.height > 1;
    let text_h = body.height.saturating_sub(u16::from(show_hbar));
    let text_area = Rect::new(body.x, body.y, text_w, text_h);
    app.editor.content_area = text_area;
    app.editor.viewport = text_h as usize;
    app.editor.viewport_w = text_w as usize;

    let vis = scroll..(scroll + text_area.height as usize).min(total);
    let nums: Vec<Line> = if let Some((ow, nw)) = diff_widths {
        // Dual gutter: old line number (blank for added rows) │ new line number (blank for removed).
        let rows = app
            .editor
            .active_buffer()
            .and_then(|b| b.diff_rows.as_ref());
        let (ow, nw) = (ow as usize, nw as usize);
        vis.map(|i| {
            let row = rows.and_then(|r| r.get(i));
            let (kind, old, new) = row
                .map(|r| (r.kind, r.old, r.new))
                .unwrap_or((DiffKind::Context, None, None));
            let fg = match kind {
                DiffKind::Add => dark().diff_add_fg,
                DiffKind::Del => dark().diff_del_fg,
                DiffKind::Context => dark().dim,
            };
            let fmt = |n: Option<usize>, w: usize| match n {
                Some(v) => format!("{v:>w$}"),
                None => " ".repeat(w),
            };
            Line::from(Span::styled(
                format!(" {} {} ", fmt(old, ow), fmt(new, nw)),
                Style::default().fg(fg),
            ))
        })
        .collect()
    } else {
        let dw = digits as usize;
        vis.map(|i| {
            let style = if i == cursor_line && focused {
                Style::default().fg(dark().accent)
            } else {
                Style::default().fg(dark().dim)
            };
            Line::from(Span::styled(format!("{:>dw$} ", i + 1), style))
        })
        .collect()
    };
    frame.render_widget(Paragraph::new(nums), gutter);

    if let Some(buf) = app.editor.active_buffer() {
        let start = buf.scroll.min(total.saturating_sub(1));
        let end = (start + text_area.height as usize).min(total);
        let visible: Vec<Line> = buf.lines[start..end].to_vec();
        frame.render_widget(
            Paragraph::new(visible).scroll((0, buf.hscroll as u16)),
            text_area,
        );
    }

    // Diff buffers: paint full-width green/red line backgrounds (added/removed). For a full-file
    // inline diff the per-row kind drives the background (so syntax-highlighted fg is preserved);
    // older patch-style buffers fall back to keying off the leading +/- char.
    if let Some(buf) = app.editor.active_buffer() {
        if buf.is_diff {
            let start = buf.scroll.min(total.saturating_sub(1));
            let end = (start + text_area.height as usize).min(total);
            for li in start..end {
                let bg = if let Some(rows) = &buf.diff_rows {
                    match rows.get(li).map(|r| r.kind) {
                        Some(DiffKind::Add) => Some(dark().diff_add_bg),
                        Some(DiffKind::Del) => Some(dark().diff_del_bg),
                        _ => None,
                    }
                } else {
                    match buf.line_text(li).chars().next() {
                        Some('+') => Some(dark().diff_add_bg),
                        Some('-') => Some(dark().diff_del_bg),
                        _ => None,
                    }
                };
                let Some(bg) = bg else { continue };
                let y = text_area.y + (li - buf.scroll) as u16;
                frame
                    .buffer_mut()
                    .set_style(Rect::new(text_area.x, y, text_area.width, 1), Style::default().bg(bg));
            }
        }
    }
    draw_selection(frame, app, text_area);
    draw_find_matches(frame, app, text_area);

    // Block cursor: blinking accent when focused, dim static otherwise. Drawn ourselves (rather
    // than the hardware cursor) so the blink is uniform with the terminal pane.
    if let Some(buf) = app.editor.active_buffer() {
        let (cl, cc) = buf.cursor;
        let col = cc.saturating_sub(buf.hscroll);
        let on_screen = cl >= buf.scroll
            && cl < buf.scroll + text_area.height as usize
            && cc >= buf.hscroll
            && (col as u16) < text_area.width;
        if on_screen {
            let show = if focused { app.blink_on } else { true };
            if show {
                let y = text_area.y + (cl - buf.scroll) as u16;
                let x = text_area.x + col as u16;
                let style = if focused {
                    Style::default().bg(dark().accent).fg(Color::Black)
                } else {
                    Style::default().bg(dark().dim).fg(Color::Black)
                };
                frame
                    .buffer_mut()
                    .set_style(Rect::new(x, y, 1, 1), style);
            }
        }
    }

    app.editor.scrollbar_col = if show_sb {
        Some(body.x + body.width - 1)
    } else {
        None
    };
    if show_sb {
        // Vertical scrollbar spans only the text height (not the hbar corner).
        draw_scrollbar(frame, Rect::new(body.x, body.y, body.width, text_h), total, scroll);
    }

    if show_hbar {
        let y = body.y + text_h;
        let track = text_w as usize;
        let max_scroll = max_w.saturating_sub(track);
        let thumb = (track * track / max_w.max(1)).clamp(1, track);
        let denom = track.saturating_sub(thumb).max(1);
        let thumb_x = if max_scroll > 0 {
            hscroll * denom / max_scroll
        } else {
            0
        };
        let bufm = frame.buffer_mut();
        for i in 0..track {
            let on_thumb = i >= thumb_x && i < thumb_x + thumb;
            // Full block for the thumb, matching the vertical scrollbar's thickness.
            let (sym, color) = if on_thumb {
                ("█", dark().dim)
            } else {
                ("─", dark().border)
            };
            if let Some(cell) = bufm.cell_mut((body.x + i as u16, y)) {
                cell.set_symbol(sym).set_fg(color);
            }
        }
        app.editor_hbar = Some((body.x, y, text_w, thumb, max_scroll));
    }
}

/// A custom vertical scrollbar in the rightmost column of `area`. The thumb is sized to the
/// visible fraction and sits flush at the bottom when scrolled to the end (unlike ratatui's
/// Scrollbar, which stops short).
/// Render the rendered-Markdown preview: wrapped styled lines + overlaid images, scrollable.
fn draw_markdown_preview(frame: &mut Frame, app: &mut App, content: Rect) {
    app.editor.viewport = content.height as usize;
    let h = content.height as usize;
    // Reserve a column for the scrollbar when scrollbars are enabled, so the wrap width is stable.
    let text_w = content.width.saturating_sub(u16::from(app.show_scrollbar)).max(1);
    let area = Rect::new(content.x, content.y, text_w, content.height);
    app.editor.content_area = area;

    if let Some(b) = app.editor.active_buffer_mut() {
        b.ensure_preview(text_w as usize);
    }
    if let Some(tx) = app.tx.clone() {
        app.editor.load_preview_images(&tx);
    }

    let (total, scroll) = {
        let Some(buf) = app.editor.active_buffer_mut() else {
            return;
        };
        let total = buf.preview_lines().len();
        buf.scroll = buf.scroll.min(total.saturating_sub(h));
        let scroll = buf.scroll;
        let start = scroll.min(total.saturating_sub(1));
        let end = (start + h).min(total);
        let visible: Vec<Line> = buf.preview_lines()[start..end].to_vec();
        frame.render_widget(Paragraph::new(visible), area);

        // Render each image at full size when its whole band is in view. The graphics library
        // scales to fit its area (it can't clip a partial slice), so a partially-scrolled image
        // would shrink to a corner — better to show it cleanly when fully visible.
        for img in buf.preview_images_mut() {
            let band = img.height as usize;
            if img.line >= start && img.line + band <= end {
                if let Some(proto) = img.proto.as_mut() {
                    let y = content.y + (img.line - start) as u16;
                    let rect = Rect::new(area.x, y, area.width, img.height);
                    frame.render_stateful_widget(StatefulImage::default(), rect, proto);
                }
            }
        }
        (total, scroll)
    };

    let show_sb = app.show_scrollbar && total > h;
    app.editor.scrollbar_col = show_sb.then(|| content.x + content.width - 1);
    if show_sb {
        draw_scrollbar(frame, content, total, scroll);
    }
}

/// Blend `color` toward white by fraction `f` (0 = unchanged, 1 = white) for banner shading.
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
            ("█", dark().dim)
        } else {
            ("│", dark().border)
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
    let hs = buf.hscroll;
    let height = content.height as usize;
    let view_end = hs + content.width as usize;
    let style = Style::default().bg(dark().sel_bg);
    for li in sl..=el {
        if li < scroll || li >= scroll + height {
            continue;
        }
        let line_len = buf.line_text(li).chars().count();
        let start = if li == sl { sc } else { 0 }.min(line_len);
        let end = if li == el { ec } else { line_len }.min(line_len);
        // Intersect the selection with the horizontally-visible window.
        let vstart = start.max(hs);
        let vend = end.min(view_end);
        if vend <= vstart {
            continue;
        }
        let y = content.y + (li - scroll) as u16;
        let x = content.x + (vstart - hs) as u16;
        let w = (vend - vstart) as u16;
        frame.buffer_mut().set_style(Rect::new(x, y, w, 1), style);
    }
}

/// Highlight find matches within the visible window (active match in accent).
fn draw_find_matches(frame: &mut Frame, app: &App, area: Rect) {
    let Some(s) = app.search.as_ref() else {
        return;
    };
    let qlen = s.query.chars().count();
    if qlen == 0 || s.matches.is_empty() {
        return;
    }
    let Some(buf) = app.editor.active_buffer() else {
        return;
    };
    let (scroll, hs) = (buf.scroll, buf.hscroll);
    let h = area.height as usize;
    let view_end = hs + area.width as usize;
    for (i, &(line, col)) in s.matches.iter().enumerate() {
        if line < scroll || line >= scroll + h {
            continue;
        }
        let vstart = col.max(hs);
        let vend = (col + qlen).min(view_end);
        if vend <= vstart {
            continue;
        }
        let style = if i == s.active {
            Style::default().bg(dark().accent).fg(Color::Black)
        } else {
            Style::default().bg(dark().sel_bg)
        };
        let y = area.y + (line - scroll) as u16;
        let x = area.x + (vstart - hs) as u16;
        frame
            .buffer_mut()
            .set_style(Rect::new(x, y, (vend - vstart) as u16, 1), style);
    }
}

/// A find widget at the editor's top-right showing the query and match count.
fn draw_find_bar(frame: &mut Frame, s: &Search, area: Rect) {
    let count = if s.matches.is_empty() {
        "no matches".to_string()
    } else {
        format!("{}/{}", s.active + 1, s.matches.len())
    };
    let body = format!("{}  ({count})", s.query);
    let w = (body.chars().count() as u16 + 4).clamp(24, area.width.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w + 1);
    let rect = Rect::new(x, area.y + 1, w, 3);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().accent))
        .title(Span::styled(
            " Find — Esc · ↵ next · ↑ prev ",
            Style::default().fg(dark().accent).bold(),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(s.query.clone(), Style::default().fg(dark().fg)),
            Span::styled(format!("  ({count})"), Style::default().fg(dark().dim)),
        ])),
        inner,
    );
    let cx = inner.x + (s.cursor as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((cx, inner.y));
}

/// Placeholder shown in the empty middle when the editor/terminal aren't open there. Distinguishes
/// *minimized* panes (collapsed to a rail — click to expand) from *hidden* panes (toggle to show).
fn draw_all_hidden(frame: &mut Frame, app: &App, area: Rect) {
    let km = &app.config.keymap;
    let hint = |chord: String, label: &str| {
        Line::from(vec![
            Span::styled(chord, Style::default().fg(dark().accent).bold()),
            Span::styled(format!("  {label}"), Style::default().fg(dark().dim)),
        ])
    };
    let sidebar_hidden = !app.sidebar_visible;
    let editor_hidden = !app.editor_visible;
    let panel_hidden = !app.panel.visible || app.panel.is_empty();
    let any_minimized = (app.sidebar_visible && app.sidebar_collapsed)
        || (app.editor_visible && app.editor_collapsed)
        || (app.panel.visible && !app.panel.is_empty() && app.panel_collapsed);
    let any_hidden = sidebar_hidden || editor_hidden || panel_hidden;

    let heading = match (any_minimized, any_hidden) {
        (true, false) => "Panes minimized",
        (true, true) => "Panes minimized or hidden",
        _ => "All panes hidden",
    };
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(heading, Style::default().fg(dark().fg).bold())),
        Line::from(""),
    ];
    if any_minimized {
        lines.push(Line::from(Span::styled(
            "click a rail to expand",
            Style::default().fg(dark().dim),
        )));
        lines.push(Line::from(""));
    }
    // Keyboard hints only for the panes that are fully hidden (a rail is restored by clicking it).
    if editor_hidden {
        lines.push(hint(km.toggle_editor.to_string(), "show editor"));
    }
    if panel_hidden {
        lines.push(hint(km.toggle_panel.to_string(), "show terminal panel"));
    }
    if sidebar_hidden {
        lines.push(hint(km.toggle_sidebar.to_string(), "show sidebar"));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(dark().border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        inner,
    );
}

fn draw_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    let bc = border_color(focused);
    let working = app.term_working();
    let title = if working {
        format!("TERMINAL  {}", app.spinner())
    } else {
        "TERMINAL".to_string()
    };
    let (inner, hit) = pane_label(frame, area, &title, focused, '›', app.hover);
    app.panel_collapse_hit = hit;

    // Split: terminal content on the left, a draggable vertical tab strip on the right.
    let strip_w = app
        .panel_strip_w
        .clamp(4, inner.width.saturating_sub(8).max(4));
    let [content, tabcol] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(strip_w)]).areas(inner);
    let tabblock = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(bc));
    let strip = tabblock.inner(tabcol);
    frame.render_widget(tabblock, tabcol);
    // Pad the tab list down one row so the first tab doesn't collide with the collapse button
    // sitting directly above it in the header.
    let strip = Rect::new(strip.x, strip.y + 1, strip.width, strip.height.saturating_sub(1));

    // Reserve a column for the scrollbar (house style, toggled by alt+s) when there's history.
    // No scrollbar for alternate-screen apps (vim/btop/ranger): they have no meaningful scrollback,
    // and toggling the reserved column frame-to-frame would resize their PTY and corrupt the layout.
    let (sb_off, sb_max, alt_screen) = app
        .panel
        .active()
        .map(|t| {
            let (o, m) = t.scrollback_state();
            (o, m, t.in_alt_screen())
        })
        .unwrap_or((0, 0, false));
    let show_term_sb =
        app.show_scrollbar && !alt_screen && sb_max > 0 && content.width > 1 && content.height > 1;
    let term_content = if show_term_sb {
        Rect::new(content.x, content.y, content.width - 1, content.height)
    } else {
        content
    };
    app.term_sb_col = show_term_sb.then(|| content.x + content.width - 1);

    app.panel.content_area = term_content;
    app.panel.tablist_area = strip;
    app.panel_divider_col = tabcol.x;
    app.panel_inner_right = inner.x + inner.width;

    let active = app.panel.active;
    let blink = app.blink_on;
    let nerd = app.config.icons == crate::icons::IconStyle::Nerd;
    let term_glyph = if nerd { "\u{f489}" } else { "\u{25b8}" }; //  terminal icon (▸ fallback)
    let w = strip.width as usize;
    // Thin, icon-only rows: [marker 1][icon 1][fill]. No close button — close via ctrl+x or a
    // middle-click on the row. The active row is a soft highlight bar (theme selection bg + edge).
    let fill = w.saturating_sub(2);
    let mut lines: Vec<Line> = app
        .panel
        .terms
        .iter()
        .enumerate()
        .map(|(i, _t)| {
            let is_active = i == active;
            // Left marker: a thin accent bar when active; pulses to a dot while the child works.
            let marker = if is_active && working {
                if blink { "\u{25cf}" } else { "\u{25cb}" } // ● / ○
            } else if is_active {
                "\u{258e}" // ▎ thin bar
            } else {
                " "
            };
            let row_bg = if is_active {
                Style::default().bg(dark().sel_bg)
            } else {
                Style::default()
            };
            let icon_fg = if is_active { dark().accent } else { dark().fg };
            Line::from(vec![
                Span::styled(marker, row_bg.fg(dark().accent)),
                Span::styled(term_glyph, row_bg.fg(icon_fg)),
                Span::styled(" ".repeat(fill), row_bg),
            ])
        })
        .collect();
    // A muted "+" affordance directly below the terminals (kept at row == terms.len() so the
    // click-row mapping in `handle_tablist_click` stays correct).
    let plus = if nerd { "\u{f067}" } else { "+" }; //  plus icon
    lines.push(Line::from(Span::styled(
        format!(" {plus}"),
        Style::default().fg(dark().dim),
    )));
    frame.render_widget(Paragraph::new(lines), strip);

    let blink = app.blink_on;
    if let Some(t) = app.panel.active_mut() {
        t.resize(term_content.height, term_content.width);
        // Blit alacritty's grid into the buffer; cursor blinks when focused (see `render_term`).
        let buf = frame.buffer_mut();
        t.with_term(|term| crate::terminal::render_term(term, term_content, buf, focused, blink));
    }

    // Scrollbar over the reserved column: history modelled as `max + view` lines, view top at
    // `max - offset` so the thumb sits flush at the bottom when live (offset 0).
    if show_term_sb {
        draw_scrollbar(
            frame,
            content,
            sb_max + term_content.height as usize,
            sb_max - sb_off,
        );
    }

    // Overlay the text selection (drag) by tinting the selected cells.
    if let Some((anchor, cursor)) = app.term_sel {
        let (a, b) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let sel = Style::default().bg(dark().sel_bg);
        let bufm = frame.buffer_mut();
        for r in a.0..=b.0 {
            if r >= term_content.height {
                break;
            }
            // Column span on this row: full width for interior rows, trimmed on the first/last.
            let c0 = if r == a.0 { a.1 } else { 0 };
            let c1 = if r == b.0 { b.1 } else { term_content.width.saturating_sub(1) };
            for c in c0..=c1 {
                if c < term_content.width {
                    bufm.set_style(Rect::new(term_content.x + c, term_content.y + r, 1, 1), sel);
                }
            }
        }
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let km = &app.config.keymap;
    let bg = Style::default().bg(dark().status_bg).fg(dark().status_fg);

    let focus = match app.focus {
        Focus::Sidebar => "EXPLORER",
        Focus::Editor => "EDITOR",
        Focus::Terminal => "CLAUDE",
    };
    let nerd = app.config.icons == crate::icons::IconStyle::Nerd;
    let mut left_spans = vec![
        Span::styled(format!(" {focus} "), bg.fg(dark().accent).bold()),
    ];
    // Branch segment (lualine-style): a git-branch glyph + name when in a repo.
    if !app.git_branch.is_empty() {
        let icon = if nerd { "\u{e0a0} " } else { "" };
        left_spans.push(Span::styled(format!(" {icon}{} ", app.git_branch), bg.fg(dark().dim)));
    }
    left_spans.push(Span::styled(format!(" {} ", app.status), bg));
    // A spinner while a terminal child is producing output ("working").
    if app.term_working() {
        left_spans.push(Span::styled(
            format!("{} working… ", app.spinner()),
            bg.fg(dark().accent),
        ));
    }
    let left = Line::from(left_spans);

    // Right segment: active file's type icon + name, cursor location, and progress through the file.
    let right = if let Some(b) = app.editor.active_buffer() {
        let (glyph, col) = crate::icons::file_icon(&b.name, false, false, app.config.icons);
        let (line, ch) = b.cursor;
        let total = b.line_count();
        let pct = if total <= 1 { 100 } else { line * 100 / (total - 1) };
        Line::from(vec![
            Span::styled(format!(" {glyph} "), bg.fg(col)),
            Span::styled(format!("{}  ", b.name), bg),
            Span::styled(format!("Ln {}, Col {}  ", line + 1, ch + 1), bg.fg(dark().dim)),
            Span::styled(format!("{pct}% "), bg.fg(dark().dim)),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {} help · {} sidebar · {} quit ", km.help, km.toggle_sidebar, km.quit),
            bg,
        ))
    };

    let [l, r] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)])
            .areas(area);
    frame.render_widget(Paragraph::new(left).style(bg), l);
    frame.render_widget(Paragraph::new(right).style(bg).alignment(Alignment::Right), r);
}
