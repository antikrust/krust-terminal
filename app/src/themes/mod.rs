pub mod default_themes;
pub mod theme;
pub mod theme_chooser;
pub mod theme_creator;
pub mod theme_creator_body;
pub mod theme_creator_modal;
pub mod theme_deletion_body;
pub mod theme_deletion_modal;

use krust_core::ui::theme::KrustTheme;

pub fn onboarding_theme_picker_themes() -> [KrustTheme; 4] {
    [
        default_themes::phenomenon(),
        default_themes::dark_theme(),
        default_themes::light_theme(),
        default_themes::adeberry(),
    ]
}
