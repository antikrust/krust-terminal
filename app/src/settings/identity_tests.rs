//! The avatar setting resolves to a source: a chosen (style, seed) -> a
//! generated source; an uploaded photo wins over the generated avatar; and a
//! brand-new install is assigned a random robot at startup rather than
//! rendering an empty face.

use settings::Setting as _;
use warpui::assets::asset_cache::AssetSource;
use warpui::{App, SingletonEntity as _};

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn a_fresh_install_is_assigned_a_random_robot() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        // Before the startup assignment runs there is genuinely nothing chosen.
        app.read(|ctx| assert!(IdentitySettings::selected_avatar_source(ctx).is_none()));

        app.update(IdentitySettings::assign_random_avatar_if_unset);
        let first = app.read(|ctx| {
            let settings = IdentitySettings::as_ref(ctx);
            let seed: usize = settings
                .avatar_seed
                .to_string()
                .parse()
                .expect("assigned seed is numeric");
            assert!(
                (1..=dicebear::VARIANT_SEEDS).contains(&seed),
                "assignment stays within the picker's variant range"
            );
            // The default style is untouched, so the face is a robot.
            assert_eq!(settings.avatar_style.to_string(), DEFAULT_AVATAR_STYLE);
            assert!(matches!(
                IdentitySettings::selected_avatar_source(ctx),
                Some(AssetSource::Async { .. })
            ));
            settings.avatar_seed.to_string()
        });

        // Sticky: running again keeps the same face.
        app.update(IdentitySettings::assign_random_avatar_if_unset);
        app.read(|ctx| {
            assert_eq!(IdentitySettings::as_ref(ctx).avatar_seed.to_string(), first);
        });
    })
}

#[test]
fn assignment_never_overrides_an_uploaded_photo() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_photo_path
                    .set_value("/tmp/me.png".to_string(), ctx)
                    .expect("set photo");
            });
        });
        app.update(IdentitySettings::assign_random_avatar_if_unset);
        app.read(|ctx| {
            assert!(
                IdentitySettings::as_ref(ctx).avatar_seed.is_empty(),
                "a photo user keeps their photo; no seed is invented"
            );
        });
    })
}

#[test]
fn choosing_an_avatar_resolves_to_a_generated_source() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_style
                    .set_value("bottts".to_string(), ctx)
                    .expect("set style");
                settings
                    .avatar_seed
                    .set_value("1".to_string(), ctx)
                    .expect("set seed");
            });
        });

        app.read(|ctx| {
            assert!(matches!(
                IdentitySettings::selected_avatar_source(ctx),
                Some(AssetSource::Async { .. })
            ));
        });
    })
}

#[test]
fn favorites_round_trip_and_toggle() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::toggle_favorite_avatar("bottts", "7", ctx);
            IdentitySettings::toggle_favorite_avatar("thumbs", "3", ctx);
        });
        app.read(|ctx| {
            assert_eq!(
                IdentitySettings::favorite_avatars(ctx),
                vec![
                    ("bottts".to_string(), "7".to_string()),
                    ("thumbs".to_string(), "3".to_string()),
                ]
            );
            assert!(IdentitySettings::is_favorite_avatar(ctx, "bottts", "7"));
        });

        // Toggling again unpins.
        app.update(|ctx| IdentitySettings::toggle_favorite_avatar("bottts", "7", ctx));
        app.read(|ctx| {
            assert!(!IdentitySettings::is_favorite_avatar(ctx, "bottts", "7"));
            assert_eq!(IdentitySettings::favorite_avatars(ctx).len(), 1);
        });
    })
}

#[test]
fn malformed_or_culled_favorites_are_dropped_on_read() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_favorites
                    // An unknown style, garbage, an empty half, and one good entry.
                    .set_value("no-such-style:2,garbage,:9,bottts:4".to_string(), ctx)
                    .expect("set favorites");
            });
        });
        app.read(|ctx| {
            assert_eq!(
                IdentitySettings::favorite_avatars(ctx),
                vec![("bottts".to_string(), "4".to_string())]
            );
        });
    })
}

#[test]
fn an_uploaded_photo_takes_precedence() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_seed
                    .set_value("1".to_string(), ctx)
                    .expect("set seed");
                settings
                    .avatar_photo_path
                    .set_value("/tmp/me.png".to_string(), ctx)
                    .expect("set photo");
            });
        });

        app.read(|ctx| {
            assert!(matches!(
                IdentitySettings::selected_avatar_source(ctx),
                Some(AssetSource::LocalFile { .. })
            ));
        });
    })
}
