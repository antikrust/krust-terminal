//! The menu bar: the app's own menu (hide, quit) and Window. No Edit menu — the text fields take
//! Cmd-V themselves, and a menu item would catch the shortcut first.

use krustui::actions::StandardAction;
use krustui::platform::menu::{Menu, MenuBar, MenuItem};

pub fn menu_bar(name: &str) -> MenuBar {
    MenuBar::new(vec![
        Menu::new(
            name,
            vec![
                MenuItem::Standard(StandardAction::Hide),
                MenuItem::Standard(StandardAction::HideOtherApps),
                MenuItem::Standard(StandardAction::ShowAllApps),
                MenuItem::Separator,
                MenuItem::Standard(StandardAction::Quit),
            ],
        ),
        Menu::new(
            "Window",
            vec![
                MenuItem::Standard(StandardAction::Minimize),
                MenuItem::Standard(StandardAction::Zoom),
                MenuItem::Standard(StandardAction::ToggleFullScreen),
                MenuItem::Separator,
                MenuItem::Standard(StandardAction::BringAllToFront),
            ],
        ),
    ])
}
