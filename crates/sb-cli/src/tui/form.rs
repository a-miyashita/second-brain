//! Single-line text input and forms for the terminal UI.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;

/// Display width of `s` in terminal cells.
fn width(s: &str) -> usize {
    Span::raw(s).width()
}

/// A single-line text input with a cursor.
#[derive(Debug, Clone, Default)]
pub(crate) struct Input {
    text: String,
    /// Cursor position in chars.
    cursor: usize,
    masked: bool,
}

impl Input {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Input {
            text,
            cursor,
            masked: false,
        }
    }

    /// Show `•` instead of the characters (secrets).
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn byte_at(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Apply an editing key. Returns whether the key was used.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte_at(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_at(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }

    /// The text as shown: masked or plain.
    fn shown(&self) -> Vec<String> {
        self.text
            .chars()
            .map(|c| {
                if self.masked {
                    "•".to_string()
                } else {
                    c.to_string()
                }
            })
            .collect()
    }

    /// The visible part for a field `cols` cells wide, and the cursor column.
    /// Scrolls horizontally so the cursor stays visible.
    pub fn view(&self, cols: usize) -> (String, usize) {
        let cols = cols.max(1);
        let chars = self.shown();
        let w = |range: &[String]| range.iter().map(|c| width(c)).sum::<usize>();
        let mut start = 0;
        while start < self.cursor && w(&chars[start..self.cursor]) + 1 > cols {
            start += 1;
        }
        let mut out = String::new();
        let mut used = 0;
        for c in &chars[start..] {
            let cw = width(c);
            if used + cw > cols {
                break;
            }
            used += cw;
            out.push_str(c);
        }
        (out, w(&chars[start..self.cursor]))
    }
}

/// The value part of a form field.
#[derive(Debug, Clone)]
pub(crate) enum FieldKind {
    Text(Input),
    Toggle(bool),
}

#[derive(Debug, Clone)]
pub(crate) struct Field {
    pub key: &'static str,
    pub label: String,
    pub kind: FieldKind,
    /// Shown under the form while the field has the focus.
    pub help: Option<String>,
}

impl Field {
    pub fn text(key: &'static str, label: impl Into<String>, value: impl Into<String>) -> Self {
        Field {
            key,
            label: label.into(),
            kind: FieldKind::Text(Input::new(value)),
            help: None,
        }
    }

    pub fn secret(key: &'static str, label: impl Into<String>) -> Self {
        Field {
            key,
            label: label.into(),
            kind: FieldKind::Text(Input::new("").masked()),
            help: None,
        }
    }

    pub fn toggle(key: &'static str, label: impl Into<String>, on: bool) -> Self {
        Field {
            key,
            label: label.into(),
            kind: FieldKind::Toggle(on),
            help: None,
        }
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// What a key did to a form.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FormEvent {
    None,
    Submit,
    Cancel,
}

/// A modal form: fields, then a submit button.
#[derive(Debug, Clone)]
pub(crate) struct Form {
    pub title: String,
    /// Lines shown above the fields.
    pub notes: Vec<String>,
    pub fields: Vec<Field>,
    /// Index of the focused field; `fields.len()` is the submit button.
    pub focus: usize,
    pub submit: String,
}

impl Form {
    pub fn new(title: impl Into<String>, fields: Vec<Field>) -> Self {
        Form {
            title: title.into(),
            notes: Vec::new(),
            fields,
            focus: 0,
            submit: "Save".into(),
        }
    }

    pub fn note(mut self, line: impl Into<String>) -> Self {
        self.notes.push(line.into());
        self
    }

    pub fn submit_label(mut self, label: impl Into<String>) -> Self {
        self.submit = label.into();
        self
    }

    fn slots(&self) -> usize {
        self.fields.len() + 1
    }

    pub fn handle(&mut self, key: KeyEvent) -> FormEvent {
        match key.code {
            KeyCode::Esc => return FormEvent::Cancel,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % self.slots(),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + self.slots() - 1) % self.slots()
            }
            KeyCode::Enter => {
                // A one-field form submits from the field itself.
                if self.focus >= self.fields.len() || self.fields.len() == 1 {
                    return FormEvent::Submit;
                }
                self.focus += 1;
            }
            _ => {
                let Some(field) = self.fields.get_mut(self.focus) else {
                    return FormEvent::None;
                };
                match &mut field.kind {
                    FieldKind::Text(input) => {
                        input.handle(key);
                    }
                    FieldKind::Toggle(on) => {
                        if matches!(
                            key.code,
                            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                        ) {
                            *on = !*on;
                        }
                    }
                }
            }
        }
        FormEvent::None
    }

    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// The trimmed text of a text field (empty when missing).
    pub fn text(&self, key: &str) -> String {
        match self.field(key).map(|f| &f.kind) {
            Some(FieldKind::Text(i)) => i.text().trim().to_string(),
            _ => String::new(),
        }
    }

    pub fn toggle(&self, key: &str) -> bool {
        matches!(
            self.field(key).map(|f| &f.kind),
            Some(FieldKind::Toggle(true))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(s: &str) -> Input {
        let mut i = Input::default();
        for c in s.chars() {
            i.handle(key(KeyCode::Char(c)));
        }
        i
    }

    #[test]
    fn input_edits_at_the_cursor() {
        let mut i = typed("helo");
        i.handle(key(KeyCode::Left));
        i.handle(key(KeyCode::Char('l')));
        assert_eq!(i.text(), "hello");
        i.handle(key(KeyCode::Home));
        i.handle(key(KeyCode::Delete));
        assert_eq!(i.text(), "ello");
        i.handle(key(KeyCode::End));
        i.handle(key(KeyCode::Backspace));
        assert_eq!(i.text(), "ell");
        i.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(i.text(), "");
    }

    #[test]
    fn input_handles_wide_chars_and_scrolls() {
        let i = typed("日本語abc");
        assert_eq!(i.view(20), ("日本語abc".to_string(), 9));
        // Five cells: the cursor at the end needs one, so the view starts later.
        let (shown, x) = i.view(5);
        assert_eq!(shown, "abc");
        assert_eq!(x, 3);
        assert_eq!(typed("abc").masked().view(10), ("•••".to_string(), 3));
    }

    #[test]
    fn form_moves_toggles_and_submits() {
        let mut f = Form::new(
            "t",
            vec![Field::text("a", "A", "x"), Field::toggle("b", "B", true)],
        );
        assert_eq!(f.handle(key(KeyCode::Enter)), FormEvent::None);
        assert_eq!(f.focus, 1);
        f.handle(key(KeyCode::Char(' ')));
        assert!(!f.toggle("b"));
        assert_eq!(f.handle(key(KeyCode::Enter)), FormEvent::None);
        assert_eq!(f.handle(key(KeyCode::Enter)), FormEvent::Submit);
        f.handle(key(KeyCode::Tab));
        assert_eq!(f.focus, 0);
        assert_eq!(f.handle(key(KeyCode::Esc)), FormEvent::Cancel);
        let mut one = Form::new("t", vec![Field::text("a", "A", "")]);
        assert_eq!(one.handle(key(KeyCode::Enter)), FormEvent::Submit);
    }
}
