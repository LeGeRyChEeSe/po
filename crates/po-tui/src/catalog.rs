//! The catalog browser (`/catalogue`): the community's extensions, walked with the arrows; a key
//! installs, shows, removes or votes for the one highlighted.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

/// An extension as the browser shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub description: String,
    /// Its category's key (`appearance`, `tools`…).
    pub category: String,
    pub author: String,
    pub certified: bool,
    pub votes: u64,
    pub installed: bool,
}

/// What the browser asks Po to do with the extension highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Install,
    Remove,
    Vote,
}

/// What an action did: what to tell the user, and what changed for the extension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Done {
    pub message: String,
    pub installed: Option<bool>,
    pub votes: Option<u64>,
}

/// What a key does in the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Move(i32),
    /// The previous or next category (all of them first).
    Category(i32),
    /// Its details and files, or back to the list.
    Show,
    Back,
    Act(Act),
    Close,
    None,
}

/// The browser takes the whole keyboard while it is open: a letter meant for it must never end up
/// in the conversation.
pub fn map_key(key: KeyEvent, showing: bool) -> Key {
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
        (KeyCode::Left, _) if showing => Key::Back,
        (KeyCode::Left, _) => Key::Category(-1),
        (KeyCode::Right, _) if !showing => Key::Category(1),
        (KeyCode::Enter | KeyCode::Char(' '), _) => if showing { Key::Back } else { Key::Show },
        (KeyCode::Char('i' | 'I'), false) => Key::Act(Act::Install),
        (KeyCode::Char('r' | 'R' | 'd' | 'D'), false) => Key::Act(Act::Remove),
        (KeyCode::Char('v' | 'V'), false) => Key::Act(Act::Vote),
        (KeyCode::Esc | KeyCode::Char('q'), _) if showing => Key::Back,
        (KeyCode::Esc | KeyCode::Char('q'), _) => Key::Close,
        _ => Key::None,
    }
}

/// The browser's state.
#[derive(Debug, Default)]
pub struct Browser {
    /// Every extension, in the order shown: by category, certified first, then by votes.
    pub entries: Vec<Entry>,
    /// The category shown, `None` for all of them.
    pub category: Option<String>,
    selected: usize,
    /// The extension shown in detail: its files, as (path, text).
    pub details: Option<Vec<(String, String)>>,
    /// Rows scrolled in the details.
    pub scroll: u16,
    /// What the last action said, and whether it failed.
    pub message: Option<(String, bool)>,
    /// The list is on its way.
    pub loading: bool,
    /// An action is under way: keys wait.
    pub busy: bool,
}

impl Browser {
    pub fn loading() -> Self {
        Self { loading: true, ..Self::default() }
    }

    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.loading = false;
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }

    /// The categories present, in the order of the list.
    pub fn categories(&self) -> Vec<&str> {
        let mut found: Vec<&str> = Vec::new();
        for entry in &self.entries {
            if !found.contains(&entry.category.as_str()) {
                found.push(&entry.category);
            }
        }
        found
    }

    /// The extensions of the category shown.
    pub fn visible(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| self.category.as_ref().is_none_or(|c| *c == e.category)).collect()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn current(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    /// The extension at `index` of the list shown.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.visible().len().saturating_sub(1));
        self.message = None;
    }

    pub fn move_by(&mut self, delta: i32) {
        if self.details.is_some() {
            self.scroll = (i32::from(self.scroll) + delta).clamp(0, i32::from(u16::MAX)) as u16;
            return;
        }
        let last = self.visible().len().saturating_sub(1) as i64;
        self.selected = (self.selected as i64 + i64::from(delta)).clamp(0, last) as usize;
        self.message = None;
    }

    /// All the categories, then each one, around.
    pub fn turn_category(&mut self, delta: i32) {
        let categories: Vec<String> = self.categories().into_iter().map(str::to_owned).collect();
        let count = categories.len() as i32 + 1;
        let at = self.category.as_ref().and_then(|c| categories.iter().position(|x| x == c)).map_or(0, |i| i as i32 + 1);
        let next = (at + delta).rem_euclid(count);
        self.category = (next > 0).then(|| categories[next as usize - 1].clone());
        self.selected = 0;
        self.message = None;
    }

    pub fn close_details(&mut self) {
        self.details = None;
        self.scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn entry(name: &str, category: &str) -> Entry {
        Entry {
            name: name.into(),
            title: name.into(),
            description: String::new(),
            category: category.into(),
            author: String::new(),
            certified: false,
            votes: 0,
            installed: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_arrows_walk_the_list_and_the_letters_act_on_it() {
        assert_eq!(map_key(key(KeyCode::Down), false), Key::Move(1));
        assert_eq!(map_key(key(KeyCode::Up), false), Key::Move(-1));
        assert_eq!(map_key(key(KeyCode::Right), false), Key::Category(1));
        assert_eq!(map_key(key(KeyCode::Enter), false), Key::Show);
        assert_eq!(map_key(key(KeyCode::Char('i')), false), Key::Act(Act::Install));
        assert_eq!(map_key(key(KeyCode::Char('r')), false), Key::Act(Act::Remove));
        assert_eq!(map_key(key(KeyCode::Char('d')), false), Key::Act(Act::Remove));
        assert_eq!(map_key(key(KeyCode::Char('v')), false), Key::Act(Act::Vote));
        assert_eq!(map_key(key(KeyCode::Esc), false), Key::Close);
        // In the details, Esc and ← go back to the list first.
        assert_eq!(map_key(key(KeyCode::Esc), true), Key::Back);
        assert_eq!(map_key(key(KeyCode::Left), true), Key::Back);
        assert_eq!(map_key(key(KeyCode::Char('x')), false), Key::None, "nothing leaks into the input");
    }

    #[test]
    fn the_selection_stays_in_the_list_and_follows_the_category() {
        let mut browser = Browser::loading();
        browser.set_entries(vec![entry("a", "tools"), entry("b", "tools"), entry("c", "appearance")]);
        browser.move_by(10);
        assert_eq!(browser.current().unwrap().name, "c");
        browser.move_by(-10);
        assert_eq!(browser.current().unwrap().name, "a");

        browser.turn_category(1);
        assert_eq!(browser.category.as_deref(), Some("tools"));
        assert_eq!(browser.visible().len(), 2);
        browser.turn_category(1);
        assert_eq!(browser.current().unwrap().name, "c");
        browser.turn_category(1);
        assert_eq!(browser.category, None, "all of them again");
        browser.turn_category(-1);
        assert_eq!(browser.category.as_deref(), Some("appearance"));
    }

    #[test]
    fn in_the_details_the_arrows_scroll() {
        let mut browser = Browser::loading();
        browser.set_entries(vec![entry("a", "tools"), entry("b", "tools")]);
        browser.details = Some(Vec::new());
        browser.move_by(3);
        assert_eq!((browser.scroll, browser.selected()), (3, 0));
        browser.move_by(-9);
        assert_eq!(browser.scroll, 0);
    }

    #[test]
    fn an_empty_catalog_does_not_panic() {
        let mut browser = Browser::loading();
        browser.set_entries(Vec::new());
        browser.move_by(1);
        browser.turn_category(1);
        assert!(browser.current().is_none());
    }
}
