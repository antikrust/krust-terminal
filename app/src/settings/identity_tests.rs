//! The avatar setting resolves to a source: nothing chosen -> `None` (render an
//! initial); a chosen (style, seed) -> a generated source; an uploaded photo
//! wins over the generated avatar.

use settings::Setting as _;
use warpui::assets::asset_cache::AssetSource;
use warpui::{App, SingletonEntity as _};

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn a_fresh_install_has_no_avatar_chosen() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.read(|ctx| {
            assert!(
                IdentitySettings::selected_avatar_source(ctx).is_none(),
                "an unchosen avatar renders an initial, not a face picked for you"
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
                    // A culled style, garbage, an empty half, and one good entry.
                    .set_value("croodles:2,garbage,:9,bottts:4".to_string(), ctx)
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
