//! A small modal single-line text input. Reusable for any "type something and confirm" flow;
//! currently used for creating a new file. The owner decides what to do on confirm based on
//! [`PromptKind`].

use std::path::PathBuf;

/// What a prompt is collecting input for.
pub enum PromptKind {
    /// Create a new file relative to `base`.
    NewFile { base: PathBuf },
    /// Commit the staged changes with the entered message.
    Commit,
}

pub struct Prompt {
    pub title: String,
    pub input: String,
    /// Cursor position as a char index into `input`.
    pub cursor: usize,
    pub kind: PromptKind,
}

impl Prompt {
    pub fn new_file(base: PathBuf) -> Self {
        Self {
            title: "New file".into(),
            input: String::new(),
            cursor: 0,
            kind: PromptKind::NewFile { base },
        }
    }

    pub fn commit() -> Self {
        Self {
            title: "Commit message".into(),
            input: String::new(),
            cursor: 0,
            kind: PromptKind::Commit,
        }
    }

    fn char_to_byte(&self, ci: usize) -> usize {
        self.input
            .char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let at = self.char_to_byte(self.cursor);
        self.input.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = self.char_to_byte(self.cursor - 1);
            let end = self.char_to_byte(self.cursor);
            self.input.replace_range(start..end, "");
            self.cursor -= 1;
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.input.chars().count();
    }
}
