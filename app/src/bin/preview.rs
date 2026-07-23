// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use derp_core::channel::{Channel, ChannelState};
use derp_core::features;

// Simple wrapper around derp::run() for feature preview channel builds.
fn main() -> Result<()> {
    ChannelState::set(
        ChannelState::new(
            Channel::Preview,
            derp_channel_config::load_config!("preview"),
        )
        .with_additional_features(features::PREVIEW_FLAGS)
        .with_additional_features(&[features::FeatureFlag::ForceLogin]),
    );

    derp::run()
}
