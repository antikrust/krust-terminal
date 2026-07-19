//! The avatar setting has to survive values the bundled set does not contain:
//! a fresh install (no seed), and settings written by another build.

use settings::Setting as _;
use warpui::{App, SingletonEntity as _};

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn a_fresh_install_has_no_avatar_chosen() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.read(|ctx| {
            assert_eq!(
                IdentitySettings::selected_avatar_path(ctx),
                None,
                "an unchosen avatar renders an initial, not a face picked for you"
            );
        });
    })
}

#[test]
fn choosing_a_bundled_avatar_resolves_to_its_asset() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        let style = avatar_set::STYLES.first().expect("a bundled style");
        let (seed, path) = style.variants.first().expect("a bundled variant");

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_style
                    .set_value(style.id.to_string(), ctx)
                    .expect("set style");
                settings
                    .avatar_seed
                    .set_value(seed.to_string(), ctx)
                    .expect("set seed");
            });
        });

        app.read(|ctx| {
            assert_eq!(IdentitySettings::selected_avatar_path(ctx), Some(*path));
        });
    })
}

#[test]
fn a_stale_or_hand_edited_choice_falls_back_instead_of_breaking() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(IdentitySettings::register);

        app.update(|ctx| {
            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .avatar_style
                    .set_value("a-style-from-another-build".to_string(), ctx)
                    .expect("set style");
                settings
                    .avatar_seed
                    .set_value("99".to_string(), ctx)
                    .expect("set seed");
            });
        });

        app.read(|ctx| {
            assert_eq!(IdentitySettings::selected_avatar_path(ctx), None);
        });
    })
}
