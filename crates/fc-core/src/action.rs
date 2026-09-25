//! Named actions. Everything a key, menu entry, or command palette can trigger is one of these.

use std::fmt;
use std::str::FromStr;

macro_rules! actions {
    ($($variant:ident => $id:literal, $label:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant,)* }

        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant,)*];

            /// Stable kebab-case identifier used in keymap files.
            pub fn id(self) -> &'static str {
                match self { $(Action::$variant => $id,)* }
            }

            pub fn label(self) -> &'static str {
                match self { $(Action::$variant => $label,)* }
            }
        }
    };
}

actions! {
    // panes & navigation
    SwitchPane => "switch-pane", "Switch pane";
    GoUp => "go-up", "Go to parent folder";
    GoRoot => "go-root", "Go to root folder";
    Back => "back", "Back in history";
    Forward => "forward", "Forward in history";
    FocusPath => "focus-path", "Edit path";
    Reload => "reload", "Reload";
    ToggleHidden => "toggle-hidden", "Show/hide hidden files";
    OpenInLeft => "open-in-left", "Open folder in left pane";
    OpenInRight => "open-in-right", "Open folder in right pane";
    SwapPanes => "swap-panes", "Swap panes";
    // marks
    ToggleMark => "toggle-mark", "Toggle mark";
    MarkAndDown => "mark-and-down", "Toggle mark and move down";
    MarkUp => "mark-up", "Toggle mark and move up";
    MarkDown => "mark-down", "Toggle mark and move down";
    MarkAll => "mark-all", "Mark all";
    UnmarkAll => "unmark-all", "Unmark all";
    InvertMarks => "invert-marks", "Invert marks";
    // sorting
    SortByName => "sort-by-name", "Sort by name";
    SortByExt => "sort-by-ext", "Sort by extension";
    SortBySize => "sort-by-size", "Sort by size";
    SortByDate => "sort-by-date", "Sort by date";
    // file operations
    View => "view", "View";
    Edit => "edit", "Edit";
    Copy => "copy", "Copy";
    Move => "move", "Move";
    NewFolder => "new-folder", "New folder";
    NewFile => "new-file", "New file";
    Delete => "delete", "Delete (to trash)";
    DeletePermanent => "delete-permanent", "Delete permanently";
    Rename => "rename", "Rename";
    ClipboardCopy => "clipboard-copy", "Copy to clipboard";
    ClipboardCut => "clipboard-cut", "Cut to clipboard";
    ClipboardPaste => "clipboard-paste", "Paste from clipboard";
    // app
    Quit => "quit", "Quit";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown action {0:?}")]
pub struct UnknownAction(pub String);

impl FromStr for Action {
    type Err = UnknownAction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Action::ALL
            .iter()
            .copied()
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
        for &action in Action::ALL {
            assert_eq!(action.id().parse::<Action>().unwrap(), action);
            assert!(seen.insert(action.id()), "duplicate id {}", action.id());
        }
        assert_eq!("nope".parse::<Action>(), Err(UnknownAction("nope".into())));
    }
}
