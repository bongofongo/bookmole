//! A minimal single-line text input. Multi-line note bodies are composed in `$EDITOR`, not here.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme;

#[derive(Debug, Default, Clone)]
pub struct InputState {
    pub value: String,
    cursor: usize,
}

impl InputState {
    pub fn new(initial: &str) -> Self {
        InputState {
            value: initial.to_string(),
            cursor: initial.chars().count(),
        }
    }

    fn len(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_at(&self, idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.value.insert(b, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let b = self.byte_at(self.cursor - 1);
            self.value.remove(b);
            self.cursor -= 1;
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.value)
    }
}

pub fn render(f: &mut Frame, area: Rect, prompt: &str, state: &InputState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(Span::styled(format!(" {prompt} "), theme::title()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let chars: Vec<char> = state.value.chars().collect();
    let mut spans = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        if i == state.cursor {
            spans.push(Span::styled(c.to_string(), theme::selected()));
        } else {
            spans.push(Span::raw(c.to_string()));
        }
    }
    if state.cursor >= chars.len() {
        spans.push(Span::styled(" ", theme::selected()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_track_the_cursor() {
        let mut s = InputState::new("");
        for c in "abc".chars() {
            s.insert(c);
        }
        assert_eq!(s.value, "abc");
        s.left();
        s.insert('X');
        assert_eq!(s.value, "abXc");
        s.backspace();
        assert_eq!(s.value, "abc");
        assert_eq!(s.take(), "abc");
        assert_eq!(s.value, "");
    }

    #[test]
    fn cursor_never_escapes_bounds() {
        let mut s = InputState::new("hi");
        s.right();
        s.right();
        s.right();
        s.insert('!');
        assert_eq!(s.value, "hi!");
        s.left();
        s.left();
        s.left();
        s.left();
        s.backspace();
        assert_eq!(s.value, "hi!");
    }
}
