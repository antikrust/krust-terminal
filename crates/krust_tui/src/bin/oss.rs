//! OSS-channel `krust-tui` binary and `default-run` target.
//!
//! This is what bare `cargo run -p krust_tui` builds, so it hand-builds a
//! production config and needs no internal `krust-channel-config` generator
//! (mirrors `app/src/bin/oss.rs`). It is a console application (no GUI window,
//! no app bundle), so unlike the GUI binaries it sets no `windows_subsystem`
//! attribute and embeds no `Info.plist`.

use anyhow::Result;
use krust_core::channel::{Channel, ChannelConfig, ChannelState, OzConfig, KrustServerConfig};
use krust_core::AppId;

fn main() -> Result<()> {
    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new("dev", "krust", "KrustTui"),
            logfile_name: "krust-tui.log".into(),
            server_config: KrustServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    if cfg!(debug_assertions) {
        state = state.with_additional_features(krust_core::features::DEBUG_FLAGS);
    }
    ChannelState::set(state);

    krust_tui::run()
}
