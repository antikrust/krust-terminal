//! OSS-channel `derp-tui` binary and `default-run` target.
//!
//! This is what bare `cargo run -p derp_tui` builds, so it hand-builds a
//! production config and needs no internal `derp-channel-config` generator
//! (mirrors `app/src/bin/oss.rs`). It is a console application (no GUI window,
//! no app bundle), so unlike the GUI binaries it sets no `windows_subsystem`
//! attribute and embeds no `Info.plist`.

use anyhow::Result;
use derp_core::channel::{Channel, ChannelConfig, ChannelState, OzConfig, DerpServerConfig};
use derp_core::AppId;

fn main() -> Result<()> {
    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new("dev", "derp", "DerpTui"),
            logfile_name: "derp-tui.log".into(),
            server_config: DerpServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    if cfg!(debug_assertions) {
        state = state.with_additional_features(derp_core::features::DEBUG_FLAGS);
    }
    ChannelState::set(state);

    derp_tui::run()
}
