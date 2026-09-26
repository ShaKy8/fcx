//! Named actions. Everything a key, menu entry, toolbar button, or the
//! functions bar can trigger is one of these.

use std::fmt;
use std::str::FromStr;

macro_rules! actions {
    ($($variant:ident => $id:literal, $label:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant,)* }

        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant,)*];

            /// Stable kebab-case identifier used in keymap files and GActions.
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
    GoUp => "go-up", "Parent folder";
    GoRoot => "go-root", "Root folder";
    Back => "back", "Back";
    Forward => "forward", "Forward";
    GoToFolder => "go-to-folder", "Go to folder…";
    Reload => "reload", "Refresh";
    ReloadAll => "reload-all", "Refresh both panes";
    ToggleHidden => "toggle-hidden", "Show hidden files";
    SameFolderBoth => "same-folder-both", "Same folder in both panes";
    SwapPanes => "swap-panes", "Swap panes";
    OpenTerminal => "open-terminal", "Open terminal here";
    Search => "search", "Search…";
    OpenArchive => "open-archive", "Open archive";
    Extract => "extract", "Unpack archive…";
    Pack => "pack", "Pack…";
    CompareFolders => "compare-folders", "Compare folders (mark differences)";
    SyncFolders => "sync-folders", "Synchronize folders…";
    QuickFilter => "quick-filter", "Quick filter";
    // tabs
    NewTab => "new-tab", "New tab";
    CloseTab => "close-tab", "Close tab";
    CloseOtherTabs => "close-other-tabs", "Close other tabs";
    RestoreTab => "restore-tab", "Restore closed tab";
    LastActiveTab => "last-active-tab", "Last active tab";
    NextTab => "next-tab", "Next tab";
    PrevTab => "prev-tab", "Previous tab";
    // favorites
    AddFavorite => "add-favorite", "Add current folder to favorites";
    EditFavorites => "edit-favorites", "Edit favorites…";
    FavoritesMenu => "favorites-menu", "Favorites menu";
    ToggleTree => "toggle-tree", "Folder tree";
    CalcSize => "calc-size", "Calculate folder size";
    CalcSizeAll => "calc-size-all", "Calculate all folder sizes";
    // marks
    ToggleMark => "toggle-mark", "Toggle mark";
    MarkAndDown => "mark-and-down", "Toggle mark and move down";
    MarkUp => "mark-up", "Toggle mark and move up";
    MarkDown => "mark-down", "Toggle mark and move down";
    MarkAll => "mark-all", "Select all";
    UnmarkAll => "unmark-all", "Deselect all";
    MarkPattern => "mark-pattern", "Select by pattern…";
    UnmarkPattern => "unmark-pattern", "Deselect by pattern…";
    MarkSameExt => "mark-same-ext", "Select same extension";
    UnmarkSameExt => "unmark-same-ext", "Deselect same extension";
    InvertMarks => "invert-marks", "Invert selection";
    InvertFileMarks => "invert-file-marks", "Invert selection (files only)";
    // clipboard
    ClipboardCopy => "clipboard-copy", "Copy";
    ClipboardCut => "clipboard-cut", "Cut";
    ClipboardPaste => "clipboard-paste", "Paste";
    CopyFullPaths => "copy-full-paths", "Copy full path and name";
    CopyNames => "copy-names", "Copy names";
    CopyFolderPath => "copy-folder-path", "Copy folder path";
    // sorting
    SortByName => "sort-by-name", "Sort by name";
    SortByExt => "sort-by-ext", "Sort by extension";
    SortBySize => "sort-by-size", "Sort by size";
    SortByDate => "sort-by-date", "Sort by date";
    // file operations
    Open => "open", "Open";
    OpenWith => "open-with", "Open with…";
    Properties => "properties", "Properties…";
    ChangeAttributes => "change-attributes", "Change date and attributes…";
    ContextMenu => "context-menu", "Context menu";
    View => "view", "View";
    QuickView => "quick-view", "Quick view panel";
    Edit => "edit", "Edit";
    Copy => "copy", "Copy…";
    Move => "move", "Move…";
    NewFolder => "new-folder", "New folder…";
    NewFile => "new-file", "New file…";
    Delete => "delete", "Delete";
    DeletePermanent => "delete-permanent", "Delete permanently";
    Rename => "rename", "Rename…";
    MultiRename => "multi-rename", "Multi rename…";
    UndoRename => "undo-rename", "Undo last multi rename";
    // view
    ViewList => "view-list", "List view";
    ViewDetails => "view-details", "Details view";
    ViewThumbnails => "view-thumbnails", "Thumbnails view";
    ViewCycle => "view-cycle", "Toggle view";
    ToggleSplitOrientation => "toggle-split-orientation", "Horizontal / vertical split";
    ToggleSinglePane => "toggle-single-pane", "Dual / single pane";
    ToggleFullscreen => "toggle-fullscreen", "Full screen";
    ToggleMenuBar => "toggle-menu-bar", "Menu bar";
    ToggleToolbar => "toggle-toolbar", "Toolbar";
    TogglePlacesBar => "toggle-places-bar", "Places bar";
    ToggleFunctionsBar => "toggle-functions-bar", "Functions bar";
    // app
    Settings => "settings", "Settings…";
    EditKeymap => "edit-keymap", "Edit key bindings…";
    ShowShortcuts => "show-shortcuts", "Keyboard shortcuts";
    Quit => "quit", "Exit";
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
