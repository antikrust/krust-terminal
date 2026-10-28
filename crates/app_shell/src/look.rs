//! Krust's look: its fonts (Roboto for the UI, Hack for monospace) and your Krust theme, as the
//! [`Appearance`] everything draws with — the same type Krust itself uses, so every colour, radius
//! and text size comes out the same.

use std::path::PathBuf;

use anyhow::Result;
use krust_core::ui::appearance::Appearance;
use krust_core::ui::theme::KrustTheme;
use krustui::fonts::{FamilyId, Weight};
use krustui::{AppContext, AssetProvider, SingletonEntity};

use crate::assets::ASSETS;

/// Krust's default monospace size.
const MONOSPACE_FONT_SIZE: f32 = 13.;
/// Krust's default line height (`DEFAULT_UI_LINE_HEIGHT_RATIO`).
const LINE_HEIGHT_RATIO: f32 = 1.2;

const ROBOTO: &[&str] = &[
    "bundled/fonts/roboto/Roboto-Italic.ttf",
    "bundled/fonts/roboto/Roboto-Bold.ttf",
    "bundled/fonts/roboto/Roboto-Regular.ttf",
    "bundled/fonts/roboto/Roboto-Medium.ttf",
    "bundled/fonts/roboto/RobotoFlex-Semibold.ttf",
    "bundled/fonts/roboto/Roboto-BoldItalic.ttf",
];
const HACK: &[&str] = &[
    "bundled/fonts/hack/Hack-Regular.ttf",
    "bundled/fonts/hack/Hack-Bold.ttf",
    "bundled/fonts/hack/Hack-Italic.ttf",
    "bundled/fonts/hack/Hack-BoldItalic.ttf",
];

/// Used when no theme of yours is found: Black Moss, Krust's dark custom theme.
const BLACK_MOSS: &str = "
name: Black Moss
accent: '#2F6F44'
cursor: '#48A868'
background: '#000000'
foreground: '#D6D6D6'
details: darker
terminal_colors:
  normal:
    black: '#000000'
    red: '#A45A5A'
    green: '#3E8A57'
    yellow: '#A08A55'
    blue: '#64778C'
    magenta: '#80678A'
    cyan: '#5E8580'
    white: '#D6D6D6'
  bright:
    black: '#242424'
    red: '#CC7474'
    green: '#56B875'
    yellow: '#C8AB6A'
    blue: '#8299B3'
    magenta: '#A783B3'
    cyan: '#78A9A2'
    white: '#FFFFFF'
";

fn load_family(ctx: &mut AppContext, name: &str, paths: &[&str]) -> Result<FamilyId> {
    let bytes = paths
        .iter()
        .map(|path| ASSETS.get(path).map(|data| data.to_vec()))
        .collect::<Result<Vec<_>>>()?;
    krustui::fonts::Cache::handle(ctx).update(ctx, |cache, _| cache.load_family_from_bytes(name, bytes))
}

/// Your Krust theme: the first theme file in Krust's custom-theme folder (`~/.krust/themes`), read
/// the way Krust reads it; else Black Moss.
pub fn theme() -> KrustTheme {
    let folder = dirs::home_dir().map(|home| home.join(".krust").join("themes"));
    let mut files: Vec<PathBuf> = folder
        .and_then(|folder| std::fs::read_dir(folder).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml" || extension == "yml")
        })
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match serde_yaml::from_str::<KrustTheme>(&text) {
            Ok(theme) => return theme,
            Err(error) => log::warn!("app_shell: {} is not a theme: {error}", file.display()),
        }
    }
    serde_yaml::from_str(BLACK_MOSS).expect("the built-in theme parses")
}

/// Loads Krust's fonts and your theme into the app's [`Appearance`].
pub fn init(ctx: &mut AppContext) -> Result<()> {
    let ui = load_family(ctx, "Roboto", ROBOTO)?;
    let monospace = load_family(ctx, "Hack", HACK)?;
    let password = load_family(ctx, "Password", &["bundled/fonts/password.ttf"])?;
    let appearance = Appearance::new(
        theme(),
        monospace,
        MONOSPACE_FONT_SIZE,
        Weight::Normal,
        ui,
        LINE_HEIGHT_RATIO,
        monospace,
        password,
    );
    ctx.add_singleton_model(|_| appearance);
    Ok(())
}
