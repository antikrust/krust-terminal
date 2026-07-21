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
use warpui::assets::asset_cache::AssetSource;

use crate::ui_components::dicebear;

/// The style a brand-new install starts on. Robots is DiceBear's most
/// recognisable set.
pub const DEFAULT_AVATAR_STYLE: &str = dicebear::DEFAULT_STYLE;

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
    // Fork: path to an uploaded avatar photo. When set it takes precedence over
    // the generated (style, seed) avatar. Never synced — the image is a local
    // file, so another device would only receive a broken path.
    avatar_photo_path: AvatarPhotoPathSetting {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "identity.avatar_photo_path",
        description: "Path to an uploaded avatar photo, overriding the generated avatar.",
    },
]);

impl IdentitySettings {
    /// The source for the chosen avatar: an uploaded photo if one is set,
    /// otherwise the natively generated DiceBear avatar for the stored
    /// `(style, seed)`. `None` when nothing is chosen — callers render an
    /// initial instead.
    pub fn selected_avatar_source(app: &warpui::AppContext) -> Option<AssetSource> {
        use warpui::SingletonEntity as _;
        let settings = Self::as_ref(app);
        let photo = settings.avatar_photo_path.to_string();
        if !photo.is_empty() {
            return Some(AssetSource::LocalFile {
                path: photo,
                content_version: None,
            });
        }
        if settings.avatar_seed.is_empty() {
            return None;
        }
        Some(dicebear::avatar_source(
            &settings.avatar_style,
            &settings.avatar_seed,
        ))
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
