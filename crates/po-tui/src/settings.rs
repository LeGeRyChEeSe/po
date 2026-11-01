//! Settings panel (F2) and search palette (Ctrl+K).
//!
//! The panel knows nothing about the settings themselves: the host gives it tabs and rows, each
//! tied to a `/…` command. Changing a value therefore runs the same command as the keyboard —
//! one code path for the TUI, line mode and the palette.

/// What a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Simple information.
    None,
    /// Value among choices: displayed label, command that applies it.
    Choice { options: Vec<(String, String)>, current: usize },
    /// Enter runs the command.
    Run(String),
    /// Enter closes the panel and prepares the command in the input, to be completed.
    Prompt(String),
    /// Enter closes the panel and runs the command as if typed: its menu opens, when it has one.
    Open(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    /// Explanation shown at the foot of the panel for the selected row.
    pub hint: String,
    pub action: Action,
    /// Highlighted option: its label ripples in colors (Po Labs).
    pub featured: bool,
}

impl Row {
    pub fn info(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into(), hint: String::new(), action: Action::None, featured: false }
    }

    /// Choice row; `current` is the index of the value in effect.
    pub fn choice(label: impl Into<String>, options: Vec<(String, String)>, current: usize) -> Self {
        let current = current.min(options.len().saturating_sub(1));
        let value = options.get(current).map(|(l, _)| l.clone()).unwrap_or_default();
        Self { label: label.into(), value, hint: String::new(), action: Action::Choice { options, current }, featured: false }
    }

    pub fn run(label: impl Into<String>, value: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            hint: String::new(),
            action: Action::Run(command.into()),
            featured: false,
        }
    }

    /// A row that leaves the panel for the command's own menu (the engines, for instance: too
    /// much to show as a row, and a menu walks it with the arrows).
    pub fn open(label: impl Into<String>, value: impl Into<String>, command: impl Into<String>) -> Self {
        Self { action: Action::Open(command.into()), ..Self::run(label, value, "") }
    }

    pub fn prompt(label: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            hint: String::new(),
            action: Action::Prompt(prefix.into()),
            featured: false,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn featured(mut self) -> Self {
        self.featured = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub title: String,
    pub rows: Vec<Row>,
}

/// What a key press in the panel asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Close,
    /// Run this command, then reread the settings.
    Run(String),
    /// Close and prepare the input.
    Prompt(String),
    /// Close and run this command as typed (its menu opens, if it has one).
    Open(String),
}

#[derive(Debug, Clone)]
pub struct Panel {
    pub tabs: Vec<Tab>,
    pub tab: usize,
    pub row: usize,
}

impl Panel {
    pub fn new(tabs: Vec<Tab>) -> Self {
        Self { tabs, tab: 0, row: 0 }
    }

    /// Opens on the tab with this title, if it exists.
    pub fn at(mut self, title: &str) -> Self {
        if let Some(i) = self.tabs.iter().position(|t| t.title.eq_ignore_ascii_case(title)) {
            self.tab = i;
        }
        self
    }

    /// Replaces the content while keeping the position: the changed value stays in view.
    pub fn refresh(&mut self, tabs: Vec<Tab>) {
        self.tabs = tabs;
        self.tab = self.tab.min(self.tabs.len().saturating_sub(1));
        self.row = self.row.min(self.rows().len().saturating_sub(1));
    }

    pub fn rows(&self) -> &[Row] {
        self.tabs.get(self.tab).map(|t| t.rows.as_slice()).unwrap_or(&[])
    }

    pub fn current(&self) -> Option<&Row> {
        self.rows().get(self.row)
    }

    pub fn move_row(&mut self, delta: i32) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        self.row = (self.row as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    pub fn move_tab(&mut self, delta: i32) {
        let len = self.tabs.len();
        if len == 0 {
            return;
        }
        self.tab = ((self.tab as i32 + delta).rem_euclid(len as i32)) as usize;
        self.row = 0;
    }

    /// Changes the value of a choice row; returns the command that applies it.
    pub fn cycle(&mut self, delta: i32) -> Effect {
        let Some(row) = self.tabs.get_mut(self.tab).and_then(|t| t.rows.get_mut(self.row)) else {
            return Effect::None;
        };
        if let Action::Choice { options, current } = &mut row.action {
            if options.is_empty() {
                return Effect::None;
            }
            *current = ((*current as i32 + delta).rem_euclid(options.len() as i32)) as usize;
            row.value = options[*current].0.clone();
            return Effect::Run(options[*current].1.clone());
        }
        Effect::None
    }

    /// Enter on the selected row.
    pub fn activate(&mut self) -> Effect {
        match self.current().map(|r| r.action.clone()) {
            Some(Action::Choice { .. }) => self.cycle(1),
            Some(Action::Run(command)) => Effect::Run(command),
            Some(Action::Prompt(prefix)) => Effect::Prompt(prefix),
            Some(Action::Open(command)) => Effect::Open(command),
            _ => Effect::None,
        }
    }
}

/// An entry of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Prepare this command in the input.
    Command(String),
    /// Open the panel on this row.
    Setting { tab: usize, row: usize },
}

/// Ctrl+K palette: a fuzzy search over commands and settings.
#[derive(Debug, Clone, Default)]
pub struct Finder {
    pub query: String,
    pub items: Vec<Item>,
    pub selected: usize,
}

/// Beyond this the list is no longer useful: refine by typing.
pub const FINDER_ROWS: usize = 10;

impl Finder {
    pub fn new(commands: &[(String, String)], tabs: &[Tab]) -> Self {
        let mut items: Vec<Item> = Vec::new();
        for (t, tab) in tabs.iter().enumerate() {
            for (r, row) in tab.rows.iter().enumerate() {
                if row.action == Action::None {
                    continue;
                }
                items.push(Item {
                    label: format!("{} › {}", tab.title, row.label),
                    detail: row.value.clone(),
                    target: Target::Setting { tab: t, row: r },
                });
            }
        }
        for (name, detail) in commands {
            items.push(Item { label: name.clone(), detail: detail.clone(), target: Target::Command(name.clone()) });
        }
        Self { query: String::new(), items, selected: 0 }
    }

    /// Entries matching the search, best first.
    pub fn matches(&self) -> Vec<&Item> {
        let query = fold(&self.query);
        let mut scored: Vec<(u8, usize, &Item)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let hay = fold(&format!("{} {}", item.label, item.detail));
                let label = fold(&item.label);
                let score = if query.is_empty() {
                    3
                } else if label.split([' ', '/', '›']).any(|w| w.starts_with(&query)) {
                    0
                } else if hay.contains(&query) {
                    1
                } else {
                    return None;
                };
                Some((score, i, item))
            })
            .collect();
        scored.sort_by_key(|(score, i, _)| (*score, *i));
        scored.into_iter().map(|(_, _, item)| item).collect()
    }

    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn move_by(&mut self, delta: i32) {
        let len = self.matches().len().min(FINDER_ROWS);
        if len > 0 {
            self.selected = (self.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
        }
    }

    pub fn chosen(&self) -> Option<Target> {
        self.matches().get(self.selected).map(|item| item.target.clone())
    }
}

/// Lowercase without accents: "réflexion" is found by typing "reflex".
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> Panel {
        Panel::new(vec![
            Tab {
                title: "Session".into(),
                rows: vec![
                    Row::choice(
                        "Réflexion",
                        vec![("default".into(), "/think default".into()), ("low".into(), "/think low".into())],
                        0,
                    ),
                    Row::prompt("Add a criterion", "/criteria add "),
                ],
            },
            Tab { title: "Subagents".into(), rows: vec![Row::run("Reload", "", "/agents reload")] },
        ])
    }

    #[test]
    fn changing_a_value_runs_its_command_and_wraps_around() {
        let mut p = panel();
        assert_eq!(p.cycle(1), Effect::Run("/think low".into()));
        assert_eq!(p.current().unwrap().value, "low");
        assert_eq!(p.cycle(1), Effect::Run("/think default".into()));
        assert_eq!(p.cycle(-1), Effect::Run("/think low".into()));
    }

    #[test]
    fn a_row_that_opens_a_menu_leaves_the_panel_for_its_command() {
        let mut p = Panel::new(vec![Tab { title: "Models".into(), rows: vec![Row::open("Engines", "2 found", "/engines")] }]);
        assert_eq!(p.current().unwrap().value, "2 found");
        assert_eq!(p.activate(), Effect::Open("/engines".into()));
        assert_eq!(p.cycle(1), Effect::None, "not a choice: arrows change nothing");
    }

    #[test]
    fn tabs_and_rows_stay_within_bounds() {
        let mut p = panel();
        p.move_row(10);
        assert_eq!(p.row, 1);
        assert_eq!(p.activate(), Effect::Prompt("/criteria add ".into()));
        p.move_tab(1);
        assert_eq!((p.tab, p.row), (1, 0));
        p.move_tab(1);
        assert_eq!(p.tab, 0, "tabs wrap around");
        let p = panel().at("subagents");
        assert_eq!(p.tab, 1);
    }

    #[test]
    fn the_palette_matches_without_accents_and_ranks_prefixes_first() {
        let commands = vec![("/cache".to_owned(), "server cache".to_owned()), ("/reflexion".to_owned(), "level".to_owned())];
        let mut f = Finder::new(&commands, &panel().tabs);
        for c in "reflex".chars() {
            f.type_char(c);
        }
        let found: Vec<_> = f.matches().iter().map(|i| i.label.clone()).collect();
        assert_eq!(found[0], "Session › Réflexion", "{found:?}");
        assert!(found.contains(&"/reflexion".to_owned()));
        assert_eq!(f.chosen(), Some(Target::Setting { tab: 0, row: 0 }));
        f.query = "zzz".into();
        assert!(f.matches().is_empty());
    }
}

/// The words that open the settings panel.
pub const PANEL_COMMANDS: &[&str] = &["/parametres", "/paramètres", "/reglages", "/réglages", "/settings"];

/// Does this line open a panel of the terminal (settings, subagents) rather than a menu? The
/// terminal shows its panel; a phone gets the same as menus ([`menu`]).
pub fn opens_panel(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let first = words.next().unwrap_or("");
    let rest = words.next();
    (PANEL_COMMANDS.contains(&first) && rest.is_none()) || (matches!(first, "/agents" | "/agent" | "/sous-agents") && rest.is_none())
}

/// The settings panel as menus, for a screen without one (a phone): `/settings` lists the tabs,
/// `/settings <tab>` its rows, `/settings <tab> <row>` the values of a choice. Each change comes
/// back to its tab, which shows the value now in force.
pub fn menu(tabs: &[Tab], words: &[&str]) -> Option<crate::menu::Menu> {
    use crate::menu::{Item, Menu};
    let cmd = "/settings";
    match words {
        [] => {
            let items = tabs
                .iter()
                .enumerate()
                .map(|(i, tab)| {
                    let detail = tab.rows.iter().filter(|r| !r.value.is_empty()).take(2).map(|r| format!("{} {}", r.label, r.value)).collect::<Vec<_>>().join(" · ");
                    Item::run(&tab.title, format!("{cmd} {}", i + 1)).detail(detail)
                })
                .collect();
            Some(Menu::new(po_i18n::t!("tui.settings.title"), items))
        }
        [tab] => {
            let index = tab.parse::<usize>().ok()?.checked_sub(1)?;
            let found = tabs.get(index)?;
            let here = format!("{cmd} {}", index + 1);
            let items = found
                .rows
                .iter()
                .enumerate()
                .map(|(r, row)| {
                    let item = match &row.action {
                        Action::None => Item::run(&row.label, &here),
                        Action::Choice { .. } => Item::run(&row.label, format!("{here} {}", r + 1)),
                        Action::Run(command) => Item::run(&row.label, command).then(here.clone()),
                        Action::Prompt(prefix) => Item::fill(&row.label, prefix),
                        Action::Open(command) => Item::run(&row.label, command),
                    };
                    let detail = if row.hint.is_empty() || row.value.is_empty() { format!("{}{}", row.value, row.hint) } else { format!("{} — {}", row.value, row.hint) };
                    item.detail(detail)
                })
                .collect();
            Some(Menu::new(&found.title, items).back(cmd))
        }
        [tab, row] => {
            let index = tab.parse::<usize>().ok()?.checked_sub(1)?;
            let found = tabs.get(index)?;
            let line = found.rows.get(row.parse::<usize>().ok()?.checked_sub(1)?)?;
            let Action::Choice { options, current } = &line.action else { return None };
            let back = format!("{cmd} {}", index + 1);
            let items = options
                .iter()
                .enumerate()
                .map(|(i, (label, command))| Item::run(label, command).current(i == *current).then(back.clone()))
                .collect();
            Some(Menu::new(&line.label, items).note(line.hint.clone()).back(back))
        }
        _ => None,
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn the_panel_becomes_menus_that_come_back_to_their_tab() {
        let tabs = vec![Tab {
            title: "Model".into(),
            rows: vec![
                Row::choice("Effort", vec![("low".into(), "/effort low".into()), ("high".into(), "/effort high".into())], 1),
                Row::run("Detect", "", "/engines detect"),
                Row::info("Context", "12k"),
            ],
        }];
        let top = menu(&tabs, &[]).unwrap();
        assert_eq!(top.items[0].action, crate::menu::Action::Run("/settings 1".into()));
        let tab = menu(&tabs, &["1"]).unwrap();
        assert_eq!(tab.back.as_deref(), Some("/settings"));
        assert_eq!(tab.items[0].action, crate::menu::Action::Run("/settings 1 1".into()));
        assert_eq!(tab.items[1].then.as_deref(), Some("/settings 1"));
        let choice = menu(&tabs, &["1", "1"]).unwrap();
        assert!(choice.items[1].current);
        assert_eq!(choice.items[0].action, crate::menu::Action::Run("/effort low".into()));
        assert!(menu(&tabs, &["9"]).is_none());
        assert!(opens_panel("/settings") && opens_panel("/agents") && !opens_panel("/settings 1") && !opens_panel("/agents voir"));
    }
}
