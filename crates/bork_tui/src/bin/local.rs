//! Local-channel `bork-tui` binary (internal dev builds).
//!
//! Mirrors `app/src/bin/local.rs`: loads the internal `local` channel config
//! (via the `bork-channel-config` generator) and layers the dev feature flags,
//! then hands off to the shared TUI entry point. Run it through
//! `./script/run-tui`, which installs the generator first; running it directly
//! without `bork-channel-config` on PATH will panic with install instructions.

use anyhow::Result;
use bork_core::channel::{Channel, ChannelState};
use bork_core::features;

fn main() -> Result<()> {
    ChannelState::set(
        ChannelState::new(Channel::Local, bork_channel_config::load_config!("local"))
            .with_additional_features(features::DEBUG_FLAGS)
            .with_additional_features(features::DOGFOOD_FLAGS)
            .with_additional_features(features::PREVIEW_FLAGS)
            .with_additional_features(features::LOCAL_FLAGS),
    );

    bork_tui::run()
}
