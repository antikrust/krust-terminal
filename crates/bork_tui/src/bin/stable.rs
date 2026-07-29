//! Stable-channel `bork-tui` binary.
//!
//! Mirrors `app/src/bin/stable.rs`: loads the `stable` channel config with no
//! additional feature flags, then hands off to the shared TUI entry point.

use anyhow::Result;
use bork_core::channel::{Channel, ChannelState};

fn main() -> Result<()> {
    ChannelState::set(ChannelState::new(
        Channel::Stable,
        bork_channel_config::load_config!("stable"),
    ));

    bork_tui::run()
}
