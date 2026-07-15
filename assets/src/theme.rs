use gpui::{App, Window};
use gpui_component::{Theme, ThemeRegistry};

const EMBEDDED_THEMES: &[&str] = &[
    include_str!("../themes/adventure.json"),
    include_str!("../themes/alduin.json"),
    include_str!("../themes/asciinema.json"),
    include_str!("../themes/aurora.json"),
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/everforest.json"),
    include_str!("../themes/fahrenheit.json"),
    include_str!("../themes/flexoki.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/harper.json"),
    include_str!("../themes/hybrid.json"),
    include_str!("../themes/jellybeans.json"),
    include_str!("../themes/kibble.json"),
    include_str!("../themes/macos-classic.json"),
    include_str!("../themes/matrix.json"),
    include_str!("../themes/mellifluous.json"),
    include_str!("../themes/molokai.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/spaceduck.json"),
    include_str!("../themes/tokyonight.json"),
    include_str!("../themes/twilight.json"),
];

pub(crate) fn load_embedded_themes(cx: &mut App) {
    for theme in EMBEDDED_THEMES {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(theme)
            .expect("failed to load an embedded theme");
    }
}

pub(crate) fn select_theme(theme_name: &str, window: &mut Window, cx: &mut App) {
    let Some(next_theme) = ThemeRegistry::global(cx).themes().get(theme_name).cloned() else {
        return;
    };

    let mode = next_theme.mode;
    let theme = Theme::global_mut(cx);
    if mode.is_dark() {
        theme.dark_theme = next_theme;
    } else {
        theme.light_theme = next_theme;
    }
    Theme::change(mode, Some(window), cx);
    cx.refresh_windows();
}
