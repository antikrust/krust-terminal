use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

// Settings for visibility of non-user command blocks like the bootstrap block
// and in-band command blocks.
define_settings_group!(BlockVisibilitySettings, settings: [
   should_show_bootstrap_block: ShouldShowBootstrapBlock {
       type: bool,
       default: false,
       supported_platforms: SupportedPlatforms::ALL,
       sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
       surface: settings::SettingSurfaces::GUI,
       private: false,
       toml_path: "appearance.blocks.should_show_bootstrap_block",
       description: "Whether the bootstrap block is visible in the terminal.",
   },
   should_show_in_band_command_blocks: ShouldShowInBandCommandBlocks {
       type: bool,
       default: false,
       supported_platforms: SupportedPlatforms::ALL,
       sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
       surface: settings::SettingSurfaces::GUI,
       private: false,
       toml_path: "appearance.blocks.should_show_in_band_command_blocks",
       description: "Whether in-band command blocks are visible in the terminal.",
   },
   should_show_ssh_block: ShouldShowSSHBlock {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.blocks.should_show_ssh_block",
        description: "Whether the SSH connection block is visible in the terminal.",
   },
   // Fork: the "Add useless information" toggle. Shows a per-block footer with
   // that block's memory usage (grid vs flat storage) — genuinely useless to
   // anyone who isn't debugging the renderer, which is the joke. Off by default.
   // Replaces the old dogfood-only `DebugSettings::show_memory_stats`; the
   // footer plumbing still calls the flag `show_memory_stats` internally.
   should_show_useless_information: ShouldShowUselessInformation {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.blocks.should_show_useless_information",
        description: "Whether to show a per-block memory-usage footer (grid vs flat storage).",
   }
]);
