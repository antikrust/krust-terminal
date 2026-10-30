//! Shimmering Krust loading text - renders Krust logo with shimmering text for loading states.

use krust_core::ui::appearance::Appearance;
use krustui::elements::shimmering_text::{
    ShimmerConfig, ShimmeringTextElement, ShimmeringTextStateHandle,
};
use krustui::elements::Element;
use krustui::{AppContext, SingletonEntity};

/// Krust icon glyph character
const KRUST_GLYPH: &str = "\u{E500}";

/// What sits between the icon and the text.
const ICON_GAP: &str = "  ";

/// Brackets an icon placed in front of a label (see [`with_icon`]); never shown.
const ICON_MARK: char = '\u{E5FF}';

/// Icons for the rotating local-run messages (plain symbols, no colour emoji).
pub const ICON_RUNNING: &str = "\u{2699}\u{FE0E}";
pub const ICON_CHEWING: &str = "\u{2736}";
pub const ICON_STILL_GOING: &str = "\u{25D4}";
pub const ICON_DRAWING: &str = "\u{270E}";
pub const ICON_LOADING: &str = "\u{25D0}";

/// A label that shows `icon` in front of it instead of the Krust glyph.
pub fn with_icon(icon: &str, text: &str) -> String {
    format!("{ICON_MARK}{icon}{ICON_MARK}{text}")
}

/// The icon a label asks for, if any, and the label without it.
pub fn split_icon(label: &str) -> (Option<&str>, &str) {
    let Some(rest) = label.strip_prefix(ICON_MARK) else {
        return (None, label);
    };
    match rest.split_once(ICON_MARK) {
        Some((icon, text)) => (Some(icon), text),
        None => (None, label),
    }
}

/// Creates a shimmering text element with the Krust glyph.
pub fn shimmering_krust_loading_text(
    text: impl Into<String>,
    font_size: f32,
    shimmer_handle: ShimmeringTextStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    // Use same colors as common.rs for consistency
    let base_color = theme.disabled_text_color(theme.surface_1()).into_solid();
    let shimmer_color = theme.main_text_color(theme.surface_1()).into_solid();

    // Hardcoded shimmer config for consistent animation
    let config = ShimmerConfig::default();

    // Create a single shimmering element with glyph and text
    let text = text.into();
    let (icon, text) = split_icon(&text);
    ShimmeringTextElement::new(
        format!("{}{ICON_GAP}{text}", icon.unwrap_or(KRUST_GLYPH)),
        appearance.ui_font_family(),
        font_size,
        base_color,
        shimmer_color,
        config,
        shimmer_handle,
    )
    .finish()
}
