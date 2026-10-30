use settings::{define_settings_group, SupportedPlatforms, SyncToCloud};

// Fork: whether local agent conversations are also written to text files that scripts and other
// agents can read. See `ai::conversation_transcripts`.
define_settings_group!(ConversationTranscriptSettings, settings: [
    write_conversation_transcripts: WriteConversationTranscripts {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "scripting.write_conversation_transcripts",
        description: "Whether local agent conversations and their images are kept on disk.",
    },
    // How long they are kept, in days: 0 keeps them until deleted by hand. Applied at startup.
    conversation_retention_days: ConversationRetentionDays {
        type: u16,
        default: 0,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "scripting.conversation_retention_days",
        description: "Days saved conversations and images are kept; 0 keeps them.",
    },
]);
