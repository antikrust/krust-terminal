//! Local identity: who you are to this machine, with or without an account.
//!
//! ACCOUNT-DESIGN.md phase 0 makes accounts optional and additive — the app is
//! fully usable signed out — so the pieces of "identity" that are purely
//! cosmetic live here as settings rather than on a cloud profile. An avatar is
//! exactly that: a picture you chose, meaningful offline, worth keeping
//! whether or not you ever sign in.
//!
//! An avatar is stored as the `(style, seed)` pair that generated it rather
//! than as an image, because the art is deterministic — the same pair always
//! renders the same picture. That keeps the setting two short strings, and it
//! means the choice stays portable if these ever render live instead of from
//! the bundled set.

use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

use crate::ui_components::avatar_set;

/// The style a brand-new install starts on. Robots is DiceBear's most
/// recognisable set and the wildcard picker's default too.
pub const DEFAULT_AVATAR_STYLE: &str = "bottts";

define_settings_group!(IdentitySettings, settings: [
    // The chosen art style, e.g. "bottts". Stored as a string rather than an
    // enum because the bundled set is generated (see
    // script/generate-avatars.mjs) and an unknown value must degrade to the
    // default rather than fail to deserialize.
    avatar_style: AvatarStyleSetting {
        type: String,
        default: String::from(DEFAULT_AVATAR_STYLE),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "identity.avatar_style",
        description: "The avatar art style shown for you in the app.",
    },
    // Which variant within the style. Empty means "not chosen yet", which
    // renders your initial instead of a picture — a deliberate default, so a
    // fresh install does not silently assign you a face.
    avatar_seed: AvatarSeedSetting {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "identity.avatar_seed",
        description: "Which variant of the avatar style is yours.",
    },
]);

impl IdentitySettings {
    /// The bundled asset path for the chosen avatar, or `None` when nothing is
    /// chosen yet or the stored pair no longer exists in the bundled set (an
    /// older or newer build, or a hand-edited settings file).
    pub fn selected_avatar_path(app: &warpui::AppContext) -> Option<&'static str> {
        use warpui::SingletonEntity as _;
        let settings = Self::as_ref(app);
        avatar_set::asset_path(&settings.avatar_style, &settings.avatar_seed)
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
