//! Render Markdown source into styled ratatui lines for the editor's preview mode: headings with
//! section rules, emphasis, inline/blocked code, lists, blockquotes, tables with box-drawing
//! borders, and image placeholders (the actual images are overlaid by the renderer using the
//! terminal graphics protocol — see `ImageMark`).

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// Left/right reading margin (columns) around the rendered content.
const MARGIN: usize = 2;
/// Rows reserved for each inline image (the image is fit, aspect-preserved, within this band).
pub const IMAGE_HEIGHT: u16 = 18;

/// A reserved slot where an image should be drawn over the rendered text.
#[derive(Clone)]
pub struct ImageMark {
    /// First reserved line index (0-based) in the returned lines.
    pub line: usize,
    /// Reserved height in rows.
    pub height: u16,
    /// Image source exactly as written in the document (a path or URL).
    pub source: String,
}

/// Render `source` for a pane `width` columns wide. Returns the styled lines plus the image slots.
pub fn render(source: &str, width: usize) -> (Vec<Line<'static>>, Vec<ImageMark>) {
    let width = width.max(8);
    let mut r = Render::new(width);
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(source, opts) {
        r.event(ev);
    }
    r.finish()
}

#[derive(Default)]
struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    in_head: bool,
}

struct Render {
    width: usize,
    lines: Vec<Line<'static>>,
    images: Vec<ImageMark>,
    cur: Vec<Span<'static>>,
    col: usize,
    indent: usize,
    style: Style,
    lists: Vec<Option<u64>>,
    quote: usize,
    in_code: bool,
    heading: Option<HeadingLevel>,
    // Image being described: collect its alt text for the caption.
    img: Option<(String, String)>, // (url, alt)
    // Table being built.
    table: Option<Table>,
    in_cell: bool,
    cell: String,
}

impl Render {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            images: Vec::new(),
            cur: Vec::new(),
            col: 0,
            indent: 0,
            style: Style::default().fg(theme::current().fg),
            lists: Vec::new(),
            quote: 0,
            in_code: false,
            heading: None,
            img: None,
            table: None,
            in_cell: false,
            cell: String::new(),
        }
    }

    fn finish(mut self) -> (Vec<Line<'static>>, Vec<ImageMark>) {
        self.flush();
        if self.lines.is_empty() {
            self.lines.push(Line::raw(""));
        }
        (self.lines, self.images)
    }

    /// Width available for content between the left and right margins.
    fn inner(&self) -> usize {
        self.width.saturating_sub(MARGIN * 2).max(1)
    }

    /// Right edge (column) at which text wraps.
    fn wrap_at(&self) -> usize {
        self.width.saturating_sub(MARGIN)
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        }
        self.col = 0;
    }

    fn newline(&mut self) {
        self.flush();
        self.start_prefix();
    }

    fn start_prefix(&mut self) {
        let th = theme::current();
        self.cur.push(Span::raw(" ".repeat(MARGIN)));
        self.col += MARGIN;
        for _ in 0..self.quote {
            self.cur
                .push(Span::styled("▌ ", Style::default().fg(th.accent)));
            self.col += 2;
        }
        if self.indent > 0 {
            self.cur.push(Span::raw(" ".repeat(self.indent)));
            self.col += self.indent;
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().map(line_is_blank).unwrap_or(false) {
            return;
        }
        self.lines.push(Line::raw(""));
    }

    /// Append text with word-wrapping at the right margin, in the given style.
    fn text(&mut self, s: &str, style: Style) {
        if self.col == 0 {
            self.start_prefix();
        }
        let base = MARGIN + self.indent + self.quote * 2;
        for (i, word) in s.split(' ').enumerate() {
            if word.is_empty() {
                if i > 0 && self.col < self.wrap_at() {
                    self.cur.push(Span::raw(" "));
                    self.col += 1;
                }
                continue;
            }
            let wlen = word.chars().count();
            let sep = usize::from(i > 0 && self.col > base);
            if self.col + sep + wlen > self.wrap_at() && self.col > base {
                self.newline();
            } else if sep == 1 {
                self.cur.push(Span::raw(" "));
                self.col += 1;
            }
            self.cur.push(Span::styled(word.to_string(), style));
            self.col += wlen;
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        let th = theme::current();
        // Inside a table cell, capture text instead of laying it out.
        if self.in_cell {
            match ev {
                Event::Text(t) | Event::Code(t) => self.cell.push_str(&t),
                Event::SoftBreak | Event::HardBreak => self.cell.push(' '),
                Event::End(TagEnd::TableCell) => {
                    let cell = std::mem::take(&mut self.cell);
                    if let Some(t) = self.table.as_mut() {
                        if let Some(row) = t.rows.last_mut() {
                            row.push(cell);
                        }
                    }
                    self.in_cell = false;
                }
                _ => {}
            }
            return;
        }
        // Inside an image, capture alt text for the caption.
        if let Some((_, alt)) = self.img.as_mut() {
            match ev {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => self.end_image(),
                _ => {}
            }
            return;
        }
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.in_code {
                    self.code_text(&t);
                } else {
                    let style = self.heading.map(heading_style).unwrap_or(self.style);
                    self.text(&t, style);
                }
            }
            Event::Code(c) => {
                let s = format!(" {c} ");
                let len = s.chars().count();
                if self.col + len > self.wrap_at() && self.col > MARGIN {
                    self.newline();
                }
                if self.col == 0 {
                    self.start_prefix();
                }
                self.cur
                    .push(Span::styled(s, Style::default().fg(th.accent).bg(th.sel_bg)));
                self.col += len;
            }
            Event::SoftBreak => self.text(" ", self.style),
            Event::HardBreak => self.newline(),
            Event::Rule => {
                self.blank();
                self.lines.push(rule(self.width, "─", th.border));
                self.blank();
            }
            Event::TaskListMarker(done) => {
                let m = if done { "[x] " } else { "[ ] " };
                self.text(m, Style::default().fg(th.accent));
            }
            // README-style HTML `<img src=…>` (and `<p align=center><img>…`) — not markdown image
            // syntax, but common, so pull the source out and reserve a band for it.
            Event::Html(h) | Event::InlineHtml(h) => {
                if let Some((src, alt)) = extract_img(&h) {
                    self.add_image(src, alt);
                }
            }
            _ => {}
        }
    }

    /// Reserve a band for an image plus a caption line below it.
    fn add_image(&mut self, src: String, alt: Option<String>) {
        let th = theme::current();
        self.blank();
        let line = self.lines.len();
        for _ in 0..IMAGE_HEIGHT {
            self.lines.push(Line::raw(""));
        }
        self.images.push(ImageMark {
            line,
            height: IMAGE_HEIGHT,
            source: src.clone(),
        });
        let cap = alt.filter(|a| !a.trim().is_empty()).unwrap_or(src);
        self.text(
            &format!("🖼  {cap}"),
            Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
        );
        self.flush();
        self.blank();
    }

    /// Verbatim code text (in a fenced block): full-width tinted bg, soft-wrapped.
    fn code_text(&mut self, t: &str) {
        let th = theme::current();
        let bg = Style::default().fg(th.fg).bg(th.sel_bg);
        let inner = self.inner();
        let avail = inner.saturating_sub(2).max(1);
        for (i, seg) in t.split('\n').enumerate() {
            if i > 0 {
                self.newline();
            }
            let chars: Vec<char> = seg.chars().collect();
            let chunks: Vec<String> = if chars.is_empty() {
                vec![String::new()]
            } else {
                chars.chunks(avail).map(|c| c.iter().collect()).collect()
            };
            for (j, chunk) in chunks.iter().enumerate() {
                if j > 0 {
                    self.newline();
                }
                let body = format!("  {chunk}");
                let pad = inner.max(body.chars().count());
                self.cur.push(Span::styled(format!("{body:<pad$}"), bg));
                self.col += pad;
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let th = theme::current();
        match tag {
            Tag::Heading { level, .. } => {
                self.blank();
                self.heading = Some(level);
                self.start_prefix();
                if heading_depth(level) >= 3 {
                    self.cur
                        .push(Span::styled("▎ ", Style::default().fg(th.accent)));
                    self.col += 2;
                }
            }
            Tag::Paragraph => {
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            Tag::Emphasis => self.style = self.style.add_modifier(Modifier::ITALIC),
            Tag::Strong => self.style = self.style.add_modifier(Modifier::BOLD),
            Tag::Strikethrough => self.style = self.style.add_modifier(Modifier::CROSSED_OUT),
            Tag::Link { .. } => {
                self.style = self.style.fg(th.accent).add_modifier(Modifier::UNDERLINED)
            }
            Tag::Image { dest_url, .. } => {
                self.blank();
                let line = self.lines.len();
                for _ in 0..IMAGE_HEIGHT {
                    self.lines.push(Line::raw(""));
                }
                self.images.push(ImageMark {
                    line,
                    height: IMAGE_HEIGHT,
                    source: dest_url.to_string(),
                });
                self.img = Some((dest_url.to_string(), String::new()));
            }
            Tag::BlockQuote(_) => {
                self.blank();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.blank();
                self.in_code = true;
                self.newline();
            }
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(start);
                self.indent += 2;
            }
            Tag::Item => {
                self.newline();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                let mlen = marker.chars().count();
                self.cur
                    .push(Span::styled(marker, Style::default().fg(th.accent)));
                self.col += mlen;
            }
            Tag::Table(aligns) => {
                self.blank();
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = true;
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                self.cell.clear();
                self.in_cell = true;
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(level) => {
                self.heading = None;
                self.flush();
                let th = theme::current();
                match level {
                    HeadingLevel::H1 => self.lines.push(rule(self.width, "━", th.accent)),
                    HeadingLevel::H2 => self.lines.push(rule(self.width, "─", th.border)),
                    _ => {}
                }
            }
            TagEnd::Paragraph => self.flush(),
            TagEnd::Emphasis => self.style = self.style.remove_modifier(Modifier::ITALIC),
            TagEnd::Strong => self.style = self.style.remove_modifier(Modifier::BOLD),
            TagEnd::Strikethrough => self.style = self.style.remove_modifier(Modifier::CROSSED_OUT),
            TagEnd::Link => self.style = Style::default().fg(theme::current().fg),
            TagEnd::Image => self.end_image(),
            TagEnd::BlockQuote(_) => {
                self.quote = self.quote.saturating_sub(1);
                self.flush();
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.in_code = false;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.indent = self.indent.saturating_sub(2);
                self.flush();
            }
            TagEnd::Item => self.flush(),
            TagEnd::Table => self.emit_table(),
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_head = false;
                }
            }
            _ => {}
        }
    }

    fn end_image(&mut self) {
        if let Some((url, alt)) = self.img.take() {
            let th = theme::current();
            let cap = if alt.trim().is_empty() {
                format!("🖼  {url}")
            } else {
                format!("🖼  {alt}")
            };
            self.text(&cap, Style::default().fg(th.dim).add_modifier(Modifier::ITALIC));
            self.flush();
            self.blank();
        }
    }

    /// Render the buffered table as a box-drawn grid, wrapping cell text to fit the pane.
    fn emit_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        if table.rows.is_empty() {
            return;
        }
        let th = theme::current();
        let cols = table.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 {
            return;
        }

        // Natural column widths from cell contents, then fit to the available inner width.
        let mut widths = vec![3usize; cols];
        for row in &table.rows {
            for (c, cell) in row.iter().enumerate() {
                widths[c] = widths[c].max(cell.chars().count());
            }
        }
        let avail = self.inner().saturating_sub(3 * cols + 1); // borders + 1-space padding each side
        let total: usize = widths.iter().sum();
        if total > avail {
            // Scale down proportionally, keeping a minimum of 3.
            for w in widths.iter_mut() {
                *w = (*w * avail / total.max(1)).max(3);
            }
        }

        let border = Style::default().fg(th.border);
        let push_rule = |me: &mut Self, l: &str, m: &str, r: &str| {
            let mut s = String::from(l);
            for (c, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if c + 1 < widths.len() { m } else { r });
            }
            let mut spans = vec![Span::raw(" ".repeat(MARGIN))];
            spans.push(Span::styled(s, border));
            me.lines.push(Line::from(spans));
        };

        push_rule(self, "┌", "┬", "┐");
        for (ri, row) in table.rows.iter().enumerate() {
            let is_head = ri == 0;
            // Wrap each cell to its column width; the row is as tall as the tallest cell.
            let wrapped: Vec<Vec<String>> = (0..cols)
                .map(|c| wrap_cell(row.get(c).map(String::as_str).unwrap_or(""), widths[c]))
                .collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            for line_idx in 0..height {
                let mut spans = vec![Span::raw(" ".repeat(MARGIN))];
                spans.push(Span::styled("│", border));
                for c in 0..cols {
                    let text = wrapped[c].get(line_idx).cloned().unwrap_or_default();
                    let align = table.aligns.get(c).copied().unwrap_or(Alignment::None);
                    let padded = pad_align(&text, widths[c], align);
                    let style = if is_head {
                        Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(th.fg)
                    };
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(padded, style));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled("│", border));
                }
                self.lines.push(Line::from(spans));
            }
            if is_head {
                push_rule(self, "├", "┼", "┤");
            }
        }
        push_rule(self, "└", "┴", "┘");
        self.blank();
    }
}

fn line_is_blank(l: &Line) -> bool {
    l.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Extract `(src, alt)` from an HTML fragment containing an `<img …>` tag.
fn extract_img(html: &str) -> Option<(String, Option<String>)> {
    let lower = html.to_ascii_lowercase();
    let img = lower.find("<img")?;
    let tag_end = lower[img..].find('>').map(|e| img + e).unwrap_or(html.len());
    let attr = |name: &str| -> Option<String> {
        let key = format!("{name}=");
        let rel = lower[img..tag_end].find(&key)? + key.len();
        let abs = img + rel;
        let q = *html.as_bytes().get(abs)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let start = abs + 1;
        let end = html[start..].find(q as char)? + start;
        Some(html[start..end].to_string())
    };
    Some((attr("src")?, attr("alt")))
}

/// A horizontal rule inset by the reading margin.
fn rule(width: usize, ch: &str, color: ratatui::style::Color) -> Line<'static> {
    let inner = width.saturating_sub(MARGIN * 2).max(1);
    Line::from(vec![
        Span::raw(" ".repeat(MARGIN)),
        Span::styled(ch.repeat(inner), Style::default().fg(color)),
    ])
}

fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut len = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if len > 0 && len + 1 + wlen > width {
            out.push(std::mem::take(&mut line));
            len = 0;
        }
        if len > 0 {
            line.push(' ');
            len += 1;
        }
        if wlen > width {
            // Hard-break an over-long word.
            for ch in word.chars() {
                if len == width {
                    out.push(std::mem::take(&mut line));
                    len = 0;
                }
                line.push(ch);
                len += 1;
            }
        } else {
            line.push_str(word);
            len += wlen;
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

fn pad_align(s: &str, width: usize, align: Alignment) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.chars().take(width).collect();
    }
    let pad = width - len;
    match align {
        Alignment::Right => format!("{}{s}", " ".repeat(pad)),
        Alignment::Center => {
            let l = pad / 2;
            format!("{}{s}{}", " ".repeat(l), " ".repeat(pad - l))
        }
        _ => format!("{s}{}", " ".repeat(pad)),
    }
}

fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let th = theme::current();
    let base = Style::default().add_modifier(Modifier::BOLD);
    match level {
        HeadingLevel::H1 | HeadingLevel::H2 => base.fg(th.accent),
        _ => base.fg(th.fg),
    }
}
