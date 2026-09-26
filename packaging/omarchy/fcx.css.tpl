/* fcx theme template for Omarchy. Installed to ~/.config/omarchy/themed/ and
 * rendered by Omarchy on every theme switch to
 * ~/.local/state/omarchy/current/theme/fcx.css, which fcx watches.
 * Variables come from the theme's colors.toml. */

/* Legacy named colors still honoured by GTK's default theme. */
@define-color theme_bg_color {{ background }};
@define-color theme_fg_color {{ foreground }};
@define-color theme_base_color {{ background }};
@define-color theme_text_color {{ foreground }};
@define-color theme_selected_bg_color {{ accent }};
@define-color theme_selected_fg_color {{ background }};
@define-color borders alpha({{ foreground }}, 0.18);
@define-color error_color {{ red }};
@define-color warning_color {{ yellow }};
@define-color success_color {{ green }};

/* GTK 4.16+ default theme variables. */
:root, window {
  --window-bg-color: {{ background }};
  --window-fg-color: {{ foreground }};
  --view-bg-color: {{ background }};
  --view-fg-color: {{ foreground }};
  --accent-bg-color: {{ accent }};
  --accent-fg-color: {{ background }};
  --accent-color: {{ accent }};
  --headerbar-bg-color: {{ dark_background }};
  --headerbar-fg-color: {{ foreground }};
  --popover-bg-color: {{ lighter_background }};
  --popover-fg-color: {{ foreground }};
  --dialog-bg-color: {{ lighter_background }};
  --dialog-fg-color: {{ foreground }};
  --card-bg-color: {{ lighter_background }};
  --card-fg-color: {{ foreground }};
  --sidebar-bg-color: {{ dark_background }};
  --sidebar-fg-color: {{ foreground }};
  --error-color: {{ red }};
  --warning-color: {{ yellow }};
  --success-color: {{ green }};
}

/* Surfaces */
window, .pane, menubar, popover > contents, .toolbar, .places-bar, .tab-bar,
.functions-bar, .jobs, .viewer {
  background-color: {{ background }};
  color: {{ foreground }};
}
columnview, listview, gridview, textview, textview > text, .folder-tree {
  background-color: {{ background }};
  color: {{ foreground }};
}
entry, entry > text {
  background-color: {{ dark_background }};
  color: {{ foreground }};
  border-color: alpha({{ foreground }}, 0.18);
}
columnview > header, columnview > header > button {
  background-color: {{ dark_background }};
  color: {{ muted }};
  border-color: alpha({{ foreground }}, 0.12);
}
.toolbar, .places-bar, .tab-bar, .functions-bar, .jobs {
  border-color: alpha({{ foreground }}, 0.15);
}

/* Cursor, marks, and state */
columnview > listview > row:selected, gridview > child:selected, listview > row:selected {
  background-color: {{ accent }};
  color: {{ background }};
}
.pane:not(.active) columnview > listview > row:selected,
.pane:not(.active) gridview > child:selected {
  background-color: alpha({{ accent }}, 0.35);
  color: {{ foreground }};
}
.marked { color: {{ accent }}; }
columnview > listview > row:selected .marked, gridview > child:selected .marked {
  color: {{ background }};
}
.pane.active .path-bar { border-color: {{ accent }}; }
.tab-bar button.tab:checked { background: alpha({{ accent }}, 0.25); }
.error { color: {{ red }}; }
.dim-label, .status-bar { color: {{ muted }}; }
button.suggested-action { background-color: {{ accent }}; color: {{ background }}; }
button.destructive-action { background-color: {{ red }}; color: {{ background }}; }
progressbar > trough > progress { background-color: {{ accent }}; }
menubar > item:hover, popover modelbutton:hover { background-color: alpha({{ accent }}, 0.25); }
