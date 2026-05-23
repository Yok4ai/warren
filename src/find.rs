//! In-editor find: a single-line query plus the list of matches in the active buffer.

#[derive(Default)]
pub struct Search {
    pub query: String,
    /// Input cursor as a char index into `query`.
    pub cursor: usize,
    /// Matches as `(line, char column)` of each occurrence's start.
    pub matches: Vec<(usize, usize)>,
    /// Index of the currently-focused match.
    pub active: usize,
}

impl Search {
    fn char_to_byte(&self, ci: usize) -> usize {
        self.query
            .char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let at = self.char_to_byte(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = self.char_to_byte(self.cursor - 1);
            let end = self.char_to_byte(self.cursor);
            self.query.replace_range(start..end, "");
            self.cursor -= 1;
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.query.chars().count() {
            self.cursor += 1;
        }
    }

    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.active = (self.active + 1) % self.matches.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.active = (self.active + self.matches.len() - 1) % self.matches.len();
        }
    }

    pub fn active_match(&self) -> Option<(usize, usize)> {
        self.matches.get(self.active).copied()
    }
}
