//! Named actions. Everything a key, menu entry, or command palette can trigger is one of these.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    SwitchPane,
    GoUp,
    ToggleHidden,
    FocusPath,
    Reload,
    ToggleMark,
    MarkAndDown,
    MarkUp,
    MarkDown,
    MarkAll,
    UnmarkAll,
    InvertMarks,
    MirrorPath,
    SwapPanes,
    Quit,
}

impl Action {
    pub const ALL: [Action; 15] = [
        Action::SwitchPane,
        Action::GoUp,
        Action::ToggleHidden,
        Action::FocusPath,
        Action::Reload,
        Action::ToggleMark,
        Action::MarkAndDown,
        Action::MarkUp,
        Action::MarkDown,
        Action::MarkAll,
        Action::UnmarkAll,
        Action::InvertMarks,
        Action::MirrorPath,
        Action::SwapPanes,
        Action::Quit,
    ];

    /// Stable kebab-case identifier used in keymap files.
    pub fn id(self) -> &'static str {
        match self {
            Action::SwitchPane => "switch-pane",
            Action::GoUp => "go-up",
            Action::ToggleHidden => "toggle-hidden",
            Action::FocusPath => "focus-path",
            Action::Reload => "reload",
            Action::ToggleMark => "toggle-mark",
            Action::MarkAndDown => "mark-and-down",
            Action::MarkUp => "mark-up",
            Action::MarkDown => "mark-down",
            Action::MarkAll => "mark-all",
            Action::UnmarkAll => "unmark-all",
            Action::InvertMarks => "invert-marks",
            Action::MirrorPath => "mirror-path",
            Action::SwapPanes => "swap-panes",
            Action::Quit => "quit",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::SwitchPane => "Switch pane",
            Action::GoUp => "Go to parent folder",
            Action::ToggleHidden => "Show/hide hidden files",
            Action::FocusPath => "Edit path",
            Action::Reload => "Reload",
            Action::ToggleMark => "Toggle mark",
            Action::MarkAndDown => "Toggle mark and move down",
            Action::MarkUp => "Toggle mark and move up",
            Action::MarkDown => "Toggle mark and move down",
            Action::MarkAll => "Mark all",
            Action::UnmarkAll => "Unmark all",
            Action::InvertMarks => "Invert marks",
            Action::MirrorPath => "Open this folder in the other pane",
            Action::SwapPanes => "Swap panes",
            Action::Quit => "Quit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown action {0:?}")]
pub struct UnknownAction(pub String);

impl FromStr for Action {
    type Err = UnknownAction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Action::ALL
            .into_iter()
            .find(|a| a.id() == s)
            .ok_or_else(|| UnknownAction(s.to_owned()))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for action in Action::ALL {
            assert_eq!(action.id().parse::<Action>().unwrap(), action);
            assert!(seen.insert(action.id()), "duplicate id {}", action.id());
        }
        assert_eq!("nope".parse::<Action>(), Err(UnknownAction("nope".into())));
    }
}
