//! Render-level regression tests for the avatar picker modal. The modal's
//! layout runs inside a real (headless) window so constraint bugs — like a
//! `MainAxisSize::Max` flex meeting the wrap-grid's infinite measuring
//! constraint — fail here as the debug-build panic they cause in the app.

use warp_core::ui::appearance::Appearance;
use warpui::platform::WindowStyle;
use warpui::{App, TypedActionView as _};

use super::{AvatarPickerAction, AvatarPickerModal};
use crate::settings::identity::IdentitySettings;
use crate::test_util::settings::initialize_settings_for_tests;

/// Opening the modal must survive layout with pin chips visible: a pinned
/// tile renders its chip unconditionally, exercising the same overlay path a
/// hovered tile does. This crashed (debug assert: infinite max constraint)
/// when the chip overlay was an unconstrained expanding flex.
#[test]
fn modal_lays_out_with_pinned_tiles_and_across_sections() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);
        app.add_singleton_model(|_| Appearance::mock());

        let (_, view) = app.add_window(WindowStyle::NotStealFocus, AvatarPickerModal::new);

        // Pin a tile in the default (visible) section so its chip renders
        // without needing a hover, plus one on the Saved shelf from another
        // section.
        view.update(&mut app, |modal, ctx| {
            modal.handle_action(
                &AvatarPickerAction::ToggleFavorite {
                    style: "bottts".to_string(),
                    seed: "3".to_string(),
                },
                ctx,
            );
            modal.handle_action(
                &AvatarPickerAction::ToggleFavorite {
                    style: "thumbs".to_string(),
                    seed: "5".to_string(),
                },
                ctx,
            );
        });

        // Select an avatar (selection ring path) and browse other sections,
        // including ones with background palettes; each change re-renders.
        view.update(&mut app, |modal, ctx| {
            modal.handle_action(
                &AvatarPickerAction::Select {
                    style: "bottts".to_string(),
                    seed: "3".to_string(),
                },
                ctx,
            );
        });
        for section in ["fun-emoji", "personas", "rings", "bottts"] {
            view.update(&mut app, |modal, ctx| {
                modal.handle_action(
                    &AvatarPickerAction::SelectSection(section.to_string()),
                    ctx,
                );
            });
        }

        view.update(&mut app, |modal, ctx| modal.sync_to_current_choice(ctx));

        // Sanity on the state the renders ran with.
        app.read(|ctx| {
            assert_eq!(IdentitySettings::favorite_avatars(ctx).len(), 2);
            assert!(IdentitySettings::is_favorite_avatar(ctx, "bottts", "3"));
        });
    })
}
