//! Your identity avatar, as Krust's title bar shows it: the photo you uploaded, else the generated
//! avatar for your style and seed, else your initial — read from Krust's settings
//! (`~/.krust/settings.toml`, `[identity]`), so the apps show the same person Krust does.

use krust_core::ui::appearance::Appearance;
use krustui::assets::asset_cache::AssetSource;
use krustui::elements::{
    Align, CacheOption, ConstrainedBox, Container, CornerRadius, Element, Image, Radius, Text,
};
use krustui::fonts::{Properties, Weight};
use pathfinder_color::ColorU;

use crate::dicebear;

/// The identity part of Krust's settings.
#[derive(Default)]
struct Identity {
    photo: String,
    style: String,
    seed: String,
    name: String,
}

/// Reads `[identity]` from Krust's settings file: `key = "value"` lines, nothing more needed.
fn read_identity() -> Identity {
    let mut identity = Identity::default();
    let Some(path) = dirs::home_dir().map(|home| home.join(".krust").join("settings.toml")) else {
        return identity;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return identity;
    };
    let mut in_identity = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_identity = line == "[identity]";
            continue;
        }
        if !in_identity {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "avatar_photo_path" => identity.photo = value,
            "avatar_style" => identity.style = value,
            "avatar_seed" => identity.seed = value,
            "display_name" | "name" => identity.name = value,
            _ => {}
        }
    }
    identity
}

/// The avatar, 26px as Krust draws an identity avatar: a photo in a circle, generated art with
/// softened corners, or an initial on the accent colour.
pub fn render(appearance: &Appearance) -> Box<dyn Element> {
    let identity = read_identity();
    let (source, radius) = if !identity.photo.is_empty() {
        (
            Some(AssetSource::LocalFile {
                path: identity.photo.clone(),
                content_version: None,
            }),
            Radius::Percentage(50.),
        )
    } else if !identity.seed.is_empty() {
        (
            Some(dicebear::avatar_source(&identity.style, &identity.seed)),
            Radius::Pixels(6.),
        )
    } else {
        (None, Radius::Percentage(50.))
    };
    let initial = identity
        .name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .next()
        .unwrap_or('?')
        .to_string();
    let initial = Text::new_inline(initial, appearance.ui_font_family(), 12.)
        .with_color(ColorU::black())
        .with_style(Properties::default().weight(Weight::Bold))
        .finish();
    let inner = match source {
        Some(source) => Image::new(source, CacheOption::BySize)
            .before_load(Align::new(initial).finish())
            .with_corner_radius(CornerRadius::with_all(radius))
            .finish(),
        None => Container::new(Align::new(initial).finish())
            .with_background(appearance.theme().accent())
            .with_corner_radius(CornerRadius::with_all(radius))
            .finish(),
    };
    ConstrainedBox::new(inner)
        .with_width(26.)
        .with_height(26.)
        .finish()
}
