//! OSS-channel `bork-tui` binary and `default-run` target.
//!
//! This is what bare `cargo run -p bork_tui` builds, so it hand-builds a
//! production config and needs no internal `bork-channel-config` generator
//! (mirrors `app/src/bin/oss.rs`). It is a console application (no GUI window,
//! no app bundle), so unlike the GUI binaries it sets no `windows_subsystem`
//! attribute and embeds no `Info.plist`.

use anyhow::Result;
use bork_core::channel::{Channel, ChannelConfig, ChannelState, OzConfig, BorkServerConfig};
use bork_core::AppId;

fn main() -> Result<()> {
    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new("dev", "bork", "BorkTui"),
            logfile_name: "bork-tui.log".into(),
            server_config: BorkServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    if cfg!(debug_assertions) {
        state = state.with_additional_features(bork_core::features::DEBUG_FLAGS);
    }
    ChannelState::set(state);

    bork_tui::run()
}
