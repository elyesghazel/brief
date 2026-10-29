//! Color themes: built-in palettes plus `~/.config/brief/themes/<name>.css`.
//!
//! Same palettes and color names as zunder, plus `red` and `yellow`. A theme is a block of
//! `@define-color` lines loaded above `src/style.css`. `theme = "zunder"` follows whatever theme
//! zunder is set to, so the launcher and the mail app always match.

use std::fs;
use std::path::PathBuf;

pub const COLORS: [&str; 11] =
    ["bg", "fg", "muted", "faint", "ghost", "accent", "green", "sel", "hover", "red", "yellow"];

pub const DEFAULT: &str = "graphite";

struct Builtin {
    name: &'static str,
    colors: [&'static str; 11],
}

#[rustfmt::skip]
const BUILTIN: &[Builtin] = &[
    //                           bg         fg         muted      faint      ghost      accent     green      sel        hover      red        yellow
    Builtin { name: "graphite",         colors: ["#0e0e14", "#e6e6ee", "#9a9aac", "#5a5a6a", "#34343f", "#b4befe", "#a6e3a1", "#262636", "#1b1b27", "#f38ba8", "#f9e2af"] },
    Builtin { name: "catppuccin-mocha", colors: ["#1e1e2e", "#cdd6f4", "#a6adc8", "#7f849c", "#45475a", "#cba6f7", "#a6e3a1", "#313244", "#28283a", "#f38ba8", "#f9e2af"] },
    Builtin { name: "catppuccin-latte", colors: ["#eff1f5", "#4c4f69", "#6c6f85", "#8c8fa1", "#bcc0cc", "#8839ef", "#40a02b", "#ccd0da", "#e6e9ef", "#d20f39", "#df8e1d"] },
    Builtin { name: "rose-pine",        colors: ["#191724", "#e0def4", "#908caa", "#6e6a86", "#403d52", "#c4a7e7", "#9ccfd8", "#26233a", "#1f1d2e", "#eb6f92", "#f6c177"] },
    Builtin { name: "rose-pine-dawn",   colors: ["#faf4ed", "#575279", "#797593", "#9893a5", "#cecacd", "#907aa9", "#286983", "#dfdad9", "#f2e9e1", "#b4637a", "#ea9d34"] },
    Builtin { name: "gruvbox-dark",     colors: ["#282828", "#ebdbb2", "#bdae93", "#928374", "#504945", "#fabd2f", "#b8bb26", "#3c3836", "#32302f", "#fb4934", "#fe8019"] },
    Builtin { name: "tokyo-night",      colors: ["#1a1b26", "#c0caf5", "#a9b1d6", "#565f89", "#3b4261", "#7aa2f7", "#9ece6a", "#292e42", "#1f2335", "#f7768e", "#e0af68"] },
    Builtin { name: "nord",             colors: ["#2e3440", "#eceff4", "#d8dee9", "#7b88a1", "#4c566a", "#88c0d0", "#a3be8c", "#3b4252", "#353c4a", "#bf616a", "#ebcb8b"] },
    Builtin { name: "everforest-dark",  colors: ["#2d353b", "#d3c6aa", "#9da9a0", "#7a8478", "#4f585e", "#a7c080", "#a7c080", "#3d484d", "#343f44", "#e67e80", "#dbbc7f"] },
    Builtin { name: "dracula",          colors: ["#282a36", "#f8f8f2", "#bfc4de", "#6272a4", "#44475a", "#bd93f9", "#50fa7b", "#44475a", "#343746", "#ff5555", "#f1fa8c"] },
    Builtin { name: "kanagawa",         colors: ["#1f1f28", "#dcd7ba", "#c8c093", "#727169", "#54546d", "#7e9cd8", "#98bb6c", "#2d4f67", "#2a2a37", "#e82424", "#e6c384"] },
    Builtin { name: "one-dark",         colors: ["#282c34", "#abb2bf", "#828997", "#5c6370", "#4b5263", "#61afef", "#98c379", "#3e4451", "#2c313c", "#e06c75", "#e5c07b"] },
];

/// `~/.config/brief/themes/`, one `<name>.css` per theme.
pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("brief/themes")
}

fn zunder_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("zunder/themes")
}

/// The theme zunder is set to, if zunder is installed and configured.
fn zunder_theme() -> Option<String> {
    let text = fs::read_to_string(dirs::config_dir()?.join("zunder/config.toml")).ok()?;
    let value: toml::Table = text.parse().ok()?;
    value.get("theme")?.as_str().map(str::to_owned)
}

fn css_files(dir: PathBuf) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "css"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

fn user_css(name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    let file = format!("{name}.css");
    fs::read_to_string(dir().join(&file)).or_else(|_| fs::read_to_string(zunder_dir().join(&file))).ok()
}

fn builtin_css(t: &Builtin) -> String {
    COLORS.iter().zip(t.colors).map(|(name, value)| format!("@define-color {name} {value};\n")).collect()
}

/// Every theme name: `zunder`, the built-ins, then user themes (brief's and zunder's).
pub fn names() -> Vec<String> {
    let mut names = vec!["zunder".to_owned()];
    names.extend(BUILTIN.iter().map(|t| t.name.to_owned()));
    for name in css_files(dir()).into_iter().chain(css_files(zunder_dir())) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The `@define-color` block for `name`. A user theme wins over a built-in of the same name, an
/// unknown name gives graphite. Colors a user theme leaves out come from graphite (zunder themes
/// have no `red`/`yellow`).
pub fn css(name: &str) -> String {
    let name = if name == "zunder" { zunder_theme().unwrap_or_else(|| DEFAULT.into()) } else { name.into() };
    let base = builtin_css(&BUILTIN[0]);
    if let Some(text) = user_css(&name) {
        return base + &text;
    }
    match BUILTIN.iter().find(|t| t.name == name) {
        Some(t) => builtin_css(t),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_define_every_color() {
        for t in BUILTIN {
            for v in t.colors {
                assert!(v.len() == 7 && v.starts_with('#'), "{}: {v}", t.name);
            }
        }
    }

    #[test]
    fn unknown_and_unsafe_fall_back() {
        assert_eq!(css("no-such-theme"), builtin_css(&BUILTIN[0]));
        assert_eq!(css("../etc/passwd"), builtin_css(&BUILTIN[0]));
    }
}
