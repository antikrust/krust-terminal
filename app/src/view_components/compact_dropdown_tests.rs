use krust_core::ui::appearance::Appearance;
use krustui::platform::WindowStyle;
use krustui::{App, View};

use super::{CompactDropdown, CompactDropdownItem};
use crate::menu::MenuVariant;
use crate::ui_components::icons::Icon;

#[derive(Debug, Clone, PartialEq)]
struct TestAction;

/// Baseline test that the view can render.
#[test]
fn test_render() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| Appearance::mock());
        let (_, view) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
            CompactDropdown::<TestAction>::new(MenuVariant::Fixed, ctx)
        });

        // This should not panic.
        view.read(&app, |view, ctx| view.render(ctx));

        // After adding some items, rendering should still not panic.
        view.update(&mut app, |view, ctx| {
            view.set_items(
                [
                    CompactDropdownItem::new(Icon::Folder, "Folder", TestAction),
                    CompactDropdownItem::new(Icon::Gear, "Gear", TestAction),
                ],
                ctx,
            );
        });
        view.read(&app, |view, ctx| view.render(ctx));
    })
}

#[test]
fn custom_trigger_icon_can_hide_chevron() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| Appearance::mock());
        let (_, view) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
            CompactDropdown::<TestAction>::new(MenuVariant::Fixed, ctx)
        });

        view.update(&mut app, |view, ctx| {
            view.set_trigger_icon(Icon::CaseSensitivity);
            view.set_show_trigger_chevron(false);
            view.set_items(
                [CompactDropdownItem::new(Icon::Folder, "Folder", TestAction)],
                ctx,
            );
        });

        view.read(&app, |view, ctx| {
            assert_eq!(view.trigger_icon, Some(Icon::CaseSensitivity));
            assert!(!view.show_trigger_chevron);
            view.render(ctx);
        });
    })
}
