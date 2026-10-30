//! Menus of commands: a command that needs a choice, typed without it, offers its choices in a
//! list walked with the arrows, rather than explaining which parameter to type.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

/// What choosing an item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Runs this command (a further menu opens if it still needs a choice).
    Run(String),
    /// Runs this command as it is: what it shows (a state, a list), never a menu.
    Show(String),
    /// Puts this beginning of command in the input, for the user to finish it (free text).
    Fill(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// Said on the right, discreetly.
    pub detail: String,
    pub action: Action,
    /// The value in force: marked, and where the menu opens.
    pub current: bool,
}

impl Item {
    pub fn run(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self { label: label.into(), detail: String::new(), action: Action::Run(command.into()), current: false }
    }

    pub fn show(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self { action: Action::Show(command.into()), ..Self::run(label, "") }
    }

    pub fn fill(label: impl Into<String>, start: impl Into<String>) -> Self {
        Self { action: Action::Fill(start.into()), ..Self::run(label, "") }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    /// A line above the items: the state in force, or why the menu opened.
    pub note: Option<String>,
    pub items: Vec<Item>,
    selected: usize,
}

impl Menu {
    pub fn new(title: impl Into<String>, items: Vec<Item>) -> Self {
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        Self { title: title.into(), note: None, items, selected }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        self.note = (!note.trim().is_empty()).then_some(note);
        self
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    pub fn move_by(&mut self, delta: i32) {
        let last = self.items.len().saturating_sub(1) as i64;
        self.selected = (self.selected as i64 + i64::from(delta)).clamp(0, last) as usize;
    }
}

/// What a key does in a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Move(i32),
    Choose,
    /// A digit: that item, at once.
    Pick(usize),
    Close,
    None,
}

/// The menu takes the whole keyboard while it is open.
pub fn map_key(key: KeyEvent) -> Key {
    if key.kind == KeyEventKind::Release {
        return Key::None;
    }
    let ctrl = po_term::input::is_shortcut_ctrl(key.modifiers);
    match (key.code, ctrl) {
        (KeyCode::Char('c'), true) => Key::Close,
        (KeyCode::Up, _) | (KeyCode::Char('k'), false) => Key::Move(-1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), false) => Key::Move(1),
        (KeyCode::PageUp, _) => Key::Move(-5),
        (KeyCode::PageDown, _) => Key::Move(5),
        (KeyCode::Enter | KeyCode::Right | KeyCode::Char(' '), _) => Key::Choose,
        (KeyCode::Char(c @ '1'..='9'), false) => Key::Pick(c as usize - '1' as usize),
        (KeyCode::Esc | KeyCode::Left | KeyCode::Char('q'), _) => Key::Close,
        _ => Key::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn a_menu_opens_on_the_value_in_force_and_stays_in_its_list() {
        let mut menu = Menu::new("t", vec![Item::run("a", "/x a"), Item::run("b", "/x b").current(true), Item::run("c", "/x c")]);
        assert_eq!(menu.current().unwrap().label, "b");
        menu.move_by(9);
        assert_eq!(menu.current().unwrap().label, "c");
        menu.move_by(-9);
        assert_eq!(menu.current().unwrap().label, "a");
    }

    #[test]
    fn arrows_walk_enter_chooses_a_digit_picks_and_nothing_leaks() {
        assert_eq!(map_key(key(KeyCode::Down)), Key::Move(1));
        assert_eq!(map_key(key(KeyCode::Up)), Key::Move(-1));
        assert_eq!(map_key(key(KeyCode::Enter)), Key::Choose);
        assert_eq!(map_key(key(KeyCode::Right)), Key::Choose);
        assert_eq!(map_key(key(KeyCode::Char('3'))), Key::Pick(2));
        assert_eq!(map_key(key(KeyCode::Esc)), Key::Close);
        assert_eq!(map_key(key(KeyCode::Char('x'))), Key::None);
    }

    #[test]
    fn an_empty_menu_does_not_panic() {
        let mut menu = Menu::new("t", Vec::new());
        menu.move_by(1);
        assert!(menu.current().is_none());
    }
}
