//! Local-channel `krust-tui` binary (internal dev builds).
//!
//! Mirrors `app/src/bin/local.rs`: loads the internal `local` channel config
//! (via the `krust-channel-config` generator) and layers the dev feature flags,
//! then hands off to the shared TUI entry point. Run it through
//! `./script/run-tui`, which installs the generator first; running it directly
//! without `krust-channel-config` on PATH will panic with install instructions.

use anyhow::Result;
use krust_core::channel::{Channel, ChannelState};
use krust_core::features;

fn main() -> Result<()> {
    ChannelState::set(
        ChannelState::new(Channel::Local, krust_channel_config::load_config!("local"))
            .with_additional_features(features::DEBUG_FLAGS)
            .with_additional_features(features::DOGFOOD_FLAGS)
            .with_additional_features(features::PREVIEW_FLAGS)
            .with_additional_features(features::LOCAL_FLAGS),
    );

    krust_tui::run()
}
