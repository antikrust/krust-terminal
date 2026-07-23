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
use settings::{RespectUserSyncSetting, Setting as _, SupportedPlatforms, SyncToCloud};
use derpui::assets::asset_cache::AssetSource;

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
    // Avatars you put aside while browsing the picker, as comma-separated
    // "style:seed" entries in the order you saved them. A shortlist, not a
    // selection — the chosen avatar is (avatar_style, avatar_seed).
    avatar_favorites: AvatarFavoritesSetting {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "identity.avatar_favorites",
        description: "Avatars you saved while browsing the picker.",
    },
]);

/// The most favorites the picker will keep. Old entries are not evicted; the
/// picker just stops adding, which is easier to understand than silent loss.
pub const MAX_AVATAR_FAVORITES: usize = 24;

impl IdentitySettings {
    /// The source for the chosen avatar: an uploaded photo if one is set,
    /// otherwise the natively generated DiceBear avatar for the stored
    /// `(style, seed)`. `None` when nothing is chosen — callers render an
    /// initial instead.
    pub fn selected_avatar_source(app: &derpui::AppContext) -> Option<AssetSource> {
        use derpui::SingletonEntity as _;
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

    /// A brand-new install gets a face: if no avatar was ever chosen (no seed,
    /// no photo), pick a random variant of the default style — a random robot —
    /// and save it. Runs once, right after settings load; sticky thereafter,
    /// and it never overrides a real choice, an uploaded photo, or a value
    /// arriving via settings sync.
    pub fn assign_random_avatar_if_unset(ctx: &mut derpui::AppContext) {
        use rand::Rng as _;
        use derpui::SingletonEntity as _;
        let settings = Self::as_ref(ctx);
        if !settings.avatar_seed.is_empty() || !settings.avatar_photo_path.is_empty() {
            return;
        }
        let seed = rand::thread_rng()
            .gen_range(1..=dicebear::VARIANT_SEEDS)
            .to_string();
        Self::handle(ctx).update(ctx, |settings, ctx| {
            if let Err(error) = settings.avatar_seed.set_value(seed, ctx) {
                derp_errors::report_error!(error.context("assigning a starter avatar"));
            }
        });
    }

    /// The saved-avatar shortlist as `(style, seed)` pairs, in save order.
    /// Entries whose style has since been culled from the bundle are skipped.
    pub fn favorite_avatars(app: &derpui::AppContext) -> Vec<(String, String)> {
        use derpui::SingletonEntity as _;
        parse_favorites(&Self::as_ref(app).avatar_favorites.to_string())
    }

    /// Whether `(style, seed)` is on the shortlist.
    pub fn is_favorite_avatar(app: &derpui::AppContext, style: &str, seed: &str) -> bool {
        Self::favorite_avatars(app)
            .iter()
            .any(|(s, d)| s == style && d == seed)
    }

    /// Adds `(style, seed)` to the shortlist, or removes it if already there.
    /// Adding beyond [`MAX_AVATAR_FAVORITES`] is a no-op.
    pub fn toggle_favorite_avatar(style: &str, seed: &str, ctx: &mut derpui::AppContext) {
        use derpui::SingletonEntity as _;
        let mut favorites = Self::favorite_avatars(ctx);
        let existing = favorites.iter().position(|(s, d)| s == style && d == seed);
        match existing {
            Some(index) => {
                favorites.remove(index);
            }
            None if favorites.len() >= MAX_AVATAR_FAVORITES => return,
            None => favorites.push((style.to_owned(), seed.to_owned())),
        }
        let serialized = format_favorites(&favorites);
        Self::handle(ctx).update(ctx, |settings, ctx| {
            if let Err(error) = settings.avatar_favorites.set_value(serialized, ctx) {
                derp_errors::report_error!(error.context("saving avatar favorites"));
            }
        });
    }
}

/// `"a:1,b:2"` -> `[(a, 1), (b, 2)]`. Entries without a `:`, with an empty
/// half, or naming an unbundled style are dropped rather than propagated.
fn parse_favorites(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|entry| {
            let (style, seed) = entry.trim().split_once(':')?;
            (!style.is_empty() && !seed.is_empty() && dicebear::label(style).is_some())
                .then(|| (style.to_owned(), seed.to_owned()))
        })
        .collect()
}

fn format_favorites(favorites: &[(String, String)]) -> String {
    favorites
        .iter()
        .map(|(style, seed)| format!("{style}:{seed}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
