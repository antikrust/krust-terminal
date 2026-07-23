use serde_json::{json, Value};
use strum_macros::{EnumDiscriminants, EnumIter};
use derp_core::telemetry::{EnablementState, TelemetryEvent, TelemetryEventDesc};

use crate::features::FeatureFlag;

#[derive(Debug, EnumDiscriminants)]
#[strum_discriminants(derive(EnumIter))]
pub(super) enum CliTelemetryEvent {
    /// Executing `derp agent run`
    AgentRun {
        gui: bool,
        requested_mcp_servers: usize,
        has_environment: bool,
        /// Optional task ID when running against an ambient agent task.
        task_id: Option<String>,
        /// Which execution harness was selected (e.g. "oz", "claude").
        harness: String,
    },
    /// Executing `derp agent run-ambient`
    AgentRunAmbient,
    /// Executing `derp agent profile list`
    AgentProfileList,
    /// Executing `derp agent list`
    AgentList,
    /// Executing `derp agent get`
    AgentGet,
    /// Executing `derp agent create`
    AgentCreate,
    /// Executing `derp agent update`
    AgentUpdate,
    /// Executing `derp agent delete`
    AgentDelete,
    /// Executing `derp agent skills`
    AgentSkills,
    /// Executing `derp environment list`
    EnvironmentList,
    /// Executing `derp environment create`
    EnvironmentCreate,
    /// Executing `derp environment delete`
    EnvironmentDelete,
    /// Executing `derp environment update`
    EnvironmentUpdate,
    /// Executing `derp environment get`
    EnvironmentGet,
    /// Executing `derp environment image list`
    EnvironmentImageList,
    /// Executing `derp mcp list`
    MCPList,
    /// Executing `derp model list`
    ModelList,
    /// Executing `derp memory-store list`
    MemoryStoreList,
    /// Executing `derp memory list`
    MemoryStoreListMemories,
    /// Executing `derp memory create`
    MemoryStoreCreateMemory,
    /// Executing `derp memory update`
    MemoryStoreUpdateMemory,
    /// Executing `derp memory delete`
    MemoryStoreDeleteMemory,
    /// Executing `derp memory-store get`
    MemoryStoreGetStore,
    /// Executing `derp memory-store update`
    MemoryStoreUpdateStore,
    /// Executing `derp memory-store list-store-agents`
    MemoryStoreListStoreAgents,
    /// Executing `derp memory versions`
    MemoryStoreListVersions,
    /// Executing `derp task list`
    TaskList,
    /// Executing `derp task get`
    TaskGet,
    /// Executing `derp run conversation get`
    ConversationGet,
    /// Executing `derp run get <id> --conversation`
    RunConversationGet,
    /// Executing `derp run message watch`
    RunMessageWatch { harness: &'static str },
    /// Executing `derp run message send`
    RunMessageSend { harness: &'static str },
    /// Executing `derp run message list`
    RunMessageList { harness: &'static str },
    /// Executing `derp run message read`
    RunMessageRead { harness: &'static str },
    /// Executing `derp run message mark-delivered`
    RunMessageMarkDelivered { harness: &'static str },
    /// Executing `derp login`
    Login,
    /// Executing `derp logout`
    Logout,
    /// Executing `derp whoami`
    Whoami,
    /// Executing `derp provider setup`
    ProviderSetup,
    /// Executing `derp provider list`
    ProviderList,
    /// Executing `derp integration create`
    IntegrationCreate,
    /// Executing `derp integration update`
    IntegrationUpdate,
    /// Executing `derp integration list`
    IntegrationList,
    /// Executing `derp artifact upload`
    ArtifactUpload,
    /// Executing `derp artifact get`
    ArtifactGet,
    /// Executing `derp artifact download`
    ArtifactDownload,
    /// Executing `derp api-key list`
    ApiKeyList,
    /// Executing `derp api-key create`
    ApiKeyCreate,
    /// Executing `derp api-key expire`
    ApiKeyExpire,
    /// Executing `derp schedule create`
    ScheduleCreate,
    /// Executing `derp schedule list`
    ScheduleList,
    /// Executing `derp schedule get`
    ScheduleGet,
    /// Executing `derp schedule pause`
    SchedulePause,
    /// Executing `derp schedule unpause`
    ScheduleUnpause,
    /// Executing `derp schedule update`
    ScheduleUpdate,
    /// Executing `derp schedule delete`
    ScheduleDelete,
    /// Executing `derp secret create`
    SecretCreate,
    /// Executing `derp secret delete`
    SecretDelete,
    /// Executing `derp secret update`
    SecretUpdate,
    /// Executing `derp secret list`
    SecretList,
    /// Executing `derp federate issue-token`
    FederateIssueToken,
    /// Executing `derp federate issue-gcp-token`
    FederateIssueGcpToken,
    /// Executing `derp harness-support ping`
    HarnessSupportPing,
    /// Executing `derp harness-support report-artifact`
    HarnessSupportReportArtifact { artifact_type: &'static str },
    /// Executing `derp harness-support notify-user`
    HarnessSupportNotifyUser,
    /// Executing `derp harness-support finish-task`
    HarnessSupportFinishTask { success: bool },
    /// Executing `derp harness-support report-shutdown`
    HarnessSupportReportShutdown,
}

impl TelemetryEvent for CliTelemetryEvent {
    fn name(&self) -> &'static str {
        CliTelemetryEventDiscriminants::from(self).name()
    }

    fn payload(&self) -> Option<Value> {
        match self {
            CliTelemetryEvent::AgentRun {
                gui,
                requested_mcp_servers,
                has_environment,
                task_id,
                harness,
            } => Some(json!({
                "gui": gui,
                "requested_mcp_servers": requested_mcp_servers,
                "has_environment": has_environment,
                "task_id": task_id,
                "harness": harness,
            })),
            CliTelemetryEvent::AgentRunAmbient => None,
            CliTelemetryEvent::AgentProfileList => None,
            CliTelemetryEvent::AgentList => None,
            CliTelemetryEvent::AgentGet => None,
            CliTelemetryEvent::AgentCreate => None,
            CliTelemetryEvent::AgentUpdate => None,
            CliTelemetryEvent::AgentDelete => None,
            CliTelemetryEvent::AgentSkills => None,
            CliTelemetryEvent::EnvironmentList => None,
            CliTelemetryEvent::EnvironmentCreate => None,
            CliTelemetryEvent::EnvironmentDelete => None,
            CliTelemetryEvent::EnvironmentUpdate => None,
            CliTelemetryEvent::EnvironmentGet => None,
            CliTelemetryEvent::EnvironmentImageList => None,
            CliTelemetryEvent::MCPList => None,
            CliTelemetryEvent::ModelList => None,
            CliTelemetryEvent::MemoryStoreList => None,
            CliTelemetryEvent::MemoryStoreListMemories => None,
            CliTelemetryEvent::MemoryStoreCreateMemory => None,
            CliTelemetryEvent::MemoryStoreUpdateMemory => None,
            CliTelemetryEvent::MemoryStoreDeleteMemory => None,
            CliTelemetryEvent::MemoryStoreGetStore => None,
            CliTelemetryEvent::MemoryStoreUpdateStore => None,
            CliTelemetryEvent::MemoryStoreListStoreAgents => None,
            CliTelemetryEvent::MemoryStoreListVersions => None,
            CliTelemetryEvent::TaskList => None,
            CliTelemetryEvent::TaskGet => None,
            CliTelemetryEvent::ConversationGet => None,
            CliTelemetryEvent::RunConversationGet => None,
            CliTelemetryEvent::RunMessageWatch { harness } => Some(json!({ "harness": harness })),
            CliTelemetryEvent::RunMessageSend { harness } => Some(json!({ "harness": harness })),
            CliTelemetryEvent::RunMessageList { harness } => Some(json!({ "harness": harness })),
            CliTelemetryEvent::RunMessageRead { harness } => Some(json!({ "harness": harness })),
            CliTelemetryEvent::RunMessageMarkDelivered { harness } => {
                Some(json!({ "harness": harness }))
            }
            CliTelemetryEvent::Login => None,
            CliTelemetryEvent::Logout => None,
            CliTelemetryEvent::Whoami => None,
            CliTelemetryEvent::ProviderSetup => None,
            CliTelemetryEvent::ProviderList => None,
            CliTelemetryEvent::IntegrationCreate => None,
            CliTelemetryEvent::IntegrationUpdate => None,
            CliTelemetryEvent::IntegrationList => None,
            CliTelemetryEvent::ArtifactUpload => None,
            CliTelemetryEvent::ArtifactGet => None,
            CliTelemetryEvent::ArtifactDownload => None,
            CliTelemetryEvent::ApiKeyList => None,
            CliTelemetryEvent::ApiKeyCreate => None,
            CliTelemetryEvent::ApiKeyExpire => None,
            CliTelemetryEvent::ScheduleCreate => None,
            CliTelemetryEvent::ScheduleList => None,
            CliTelemetryEvent::ScheduleGet => None,
            CliTelemetryEvent::SchedulePause => None,
            CliTelemetryEvent::ScheduleUnpause => None,
            CliTelemetryEvent::ScheduleUpdate => None,
            CliTelemetryEvent::ScheduleDelete => None,
            CliTelemetryEvent::SecretCreate => None,
            CliTelemetryEvent::SecretDelete => None,
            CliTelemetryEvent::SecretUpdate => None,
            CliTelemetryEvent::SecretList => None,
            CliTelemetryEvent::FederateIssueToken => None,
            CliTelemetryEvent::FederateIssueGcpToken => None,
            CliTelemetryEvent::HarnessSupportPing => None,
            CliTelemetryEvent::HarnessSupportReportArtifact { artifact_type } => {
                Some(json!({ "artifact_type": artifact_type }))
            }
            CliTelemetryEvent::HarnessSupportNotifyUser => None,
            CliTelemetryEvent::HarnessSupportFinishTask { success } => {
                Some(json!({ "success": success }))
            }
            CliTelemetryEvent::HarnessSupportReportShutdown => None,
        }
    }

    fn description(&self) -> &'static str {
        CliTelemetryEventDiscriminants::from(self).description()
    }

    fn enablement_state(&self) -> EnablementState {
        CliTelemetryEventDiscriminants::from(self).enablement_state()
    }

    fn contains_ugc(&self) -> bool {
        false
    }

    fn event_descs() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>> {
        derp_core::telemetry::enum_events::<Self>()
    }
}

impl TelemetryEventDesc for CliTelemetryEventDiscriminants {
    fn name(&self) -> &'static str {
        match self {
            CliTelemetryEventDiscriminants::AgentRun => "CLI.Execute.Agent.Run",
            CliTelemetryEventDiscriminants::AgentRunAmbient => "CLI.Execute.Agent.RunAmbient",
            CliTelemetryEventDiscriminants::AgentProfileList => "CLI.Execute.Agent.Profile.List",
            CliTelemetryEventDiscriminants::AgentList => "CLI.Execute.Agent.List",
            CliTelemetryEventDiscriminants::AgentGet => "CLI.Execute.Agent.Get",
            CliTelemetryEventDiscriminants::AgentCreate => "CLI.Execute.Agent.Create",
            CliTelemetryEventDiscriminants::AgentUpdate => "CLI.Execute.Agent.Update",
            CliTelemetryEventDiscriminants::AgentDelete => "CLI.Execute.Agent.Delete",
            CliTelemetryEventDiscriminants::AgentSkills => "CLI.Execute.Agent.Skills",
            CliTelemetryEventDiscriminants::EnvironmentList => "CLI.Execute.Environment.List",
            CliTelemetryEventDiscriminants::EnvironmentCreate => "CLI.Execute.Environment.Create",
            CliTelemetryEventDiscriminants::EnvironmentDelete => "CLI.Execute.Environment.Delete",
            CliTelemetryEventDiscriminants::EnvironmentUpdate => "CLI.Execute.Environment.Update",
            CliTelemetryEventDiscriminants::EnvironmentGet => "CLI.Execute.Environment.Get",
            CliTelemetryEventDiscriminants::EnvironmentImageList => {
                "CLI.Execute.Environment.Image.List"
            }
            CliTelemetryEventDiscriminants::MCPList => "CLI.Execute.MCP.List",
            CliTelemetryEventDiscriminants::ModelList => "CLI.Execute.Model.List",
            CliTelemetryEventDiscriminants::MemoryStoreList => "CLI.Execute.MemoryStore.List",
            CliTelemetryEventDiscriminants::MemoryStoreListMemories => {
                "CLI.Execute.MemoryStore.ListMemories"
            }
            CliTelemetryEventDiscriminants::MemoryStoreCreateMemory => {
                "CLI.Execute.MemoryStore.CreateMemory"
            }
            CliTelemetryEventDiscriminants::MemoryStoreUpdateMemory => {
                "CLI.Execute.MemoryStore.UpdateMemory"
            }
            CliTelemetryEventDiscriminants::MemoryStoreDeleteMemory => {
                "CLI.Execute.MemoryStore.DeleteMemory"
            }
            CliTelemetryEventDiscriminants::MemoryStoreGetStore => {
                "CLI.Execute.MemoryStore.GetStore"
            }
            CliTelemetryEventDiscriminants::MemoryStoreUpdateStore => {
                "CLI.Execute.MemoryStore.UpdateStore"
            }
            CliTelemetryEventDiscriminants::MemoryStoreListStoreAgents => {
                "CLI.Execute.MemoryStore.ListStoreAgents"
            }
            CliTelemetryEventDiscriminants::MemoryStoreListVersions => {
                "CLI.Execute.MemoryStore.ListVersions"
            }
            CliTelemetryEventDiscriminants::TaskList => "CLI.Execute.Task.List",
            CliTelemetryEventDiscriminants::TaskGet => "CLI.Execute.Task.Get",
            CliTelemetryEventDiscriminants::ConversationGet => "CLI.Execute.Conversation.Get",
            CliTelemetryEventDiscriminants::RunConversationGet => {
                "CLI.Execute.Run.Conversation.Get"
            }
            CliTelemetryEventDiscriminants::RunMessageWatch => "CLI.Execute.Run.Message.Watch",
            CliTelemetryEventDiscriminants::RunMessageSend => "CLI.Execute.Run.Message.Send",
            CliTelemetryEventDiscriminants::RunMessageList => "CLI.Execute.Run.Message.List",
            CliTelemetryEventDiscriminants::RunMessageRead => "CLI.Execute.Run.Message.Read",
            CliTelemetryEventDiscriminants::RunMessageMarkDelivered => {
                "CLI.Execute.Run.Message.MarkDelivered"
            }
            CliTelemetryEventDiscriminants::Login => "CLI.Execute.Login",
            CliTelemetryEventDiscriminants::Logout => "CLI.Execute.Logout",
            CliTelemetryEventDiscriminants::Whoami => "CLI.Execute.Whoami",
            CliTelemetryEventDiscriminants::ProviderSetup => "CLI.Execute.Provider.Setup",
            CliTelemetryEventDiscriminants::ProviderList => "CLI.Execute.Provider.List",
            CliTelemetryEventDiscriminants::IntegrationCreate => "CLI.Execute.Integration.Create",
            CliTelemetryEventDiscriminants::IntegrationUpdate => "CLI.Execute.Integration.Update",
            CliTelemetryEventDiscriminants::IntegrationList => "CLI.Execute.Integration.List",
            CliTelemetryEventDiscriminants::ArtifactUpload => "CLI.Execute.Artifact.Upload",
            CliTelemetryEventDiscriminants::ArtifactGet => "CLI.Execute.Artifact.Get",
            CliTelemetryEventDiscriminants::ArtifactDownload => "CLI.Execute.Artifact.Download",
            CliTelemetryEventDiscriminants::ApiKeyList => "CLI.Execute.ApiKey.List",
            CliTelemetryEventDiscriminants::ApiKeyCreate => "CLI.Execute.ApiKey.Create",
            CliTelemetryEventDiscriminants::ApiKeyExpire => "CLI.Execute.ApiKey.Expire",
            CliTelemetryEventDiscriminants::ScheduleCreate => "CLI.Execute.Schedule.Create",
            CliTelemetryEventDiscriminants::ScheduleList => "CLI.Execute.Schedule.List",
            CliTelemetryEventDiscriminants::ScheduleGet => "CLI.Execute.Schedule.Get",
            CliTelemetryEventDiscriminants::SchedulePause => "CLI.Execute.Schedule.Pause",
            CliTelemetryEventDiscriminants::ScheduleUnpause => "CLI.Execute.Schedule.Unpause",
            CliTelemetryEventDiscriminants::ScheduleUpdate => "CLI.Execute.Schedule.Update",
            CliTelemetryEventDiscriminants::ScheduleDelete => "CLI.Execute.Schedule.Delete",
            CliTelemetryEventDiscriminants::SecretCreate => "CLI.Execute.Secret.Create",
            CliTelemetryEventDiscriminants::SecretDelete => "CLI.Execute.Secret.Delete",
            CliTelemetryEventDiscriminants::SecretUpdate => "CLI.Execute.Secret.Update",
            CliTelemetryEventDiscriminants::SecretList => "CLI.Execute.Secret.List",
            CliTelemetryEventDiscriminants::FederateIssueToken => "CLI.Execute.Federate.IssueToken",
            CliTelemetryEventDiscriminants::FederateIssueGcpToken => {
                "CLI.Execute.Federate.IssueGcpToken"
            }
            CliTelemetryEventDiscriminants::HarnessSupportPing => "CLI.Execute.HarnessSupport.Ping",
            CliTelemetryEventDiscriminants::HarnessSupportReportArtifact => {
                "CLI.Execute.HarnessSupport.ReportArtifact"
            }
            CliTelemetryEventDiscriminants::HarnessSupportNotifyUser => {
                "CLI.Execute.HarnessSupport.NotifyUser"
            }
            CliTelemetryEventDiscriminants::HarnessSupportFinishTask => {
                "CLI.Execute.HarnessSupport.FinishTask"
            }
            CliTelemetryEventDiscriminants::HarnessSupportReportShutdown => {
                "CLI.Execute.HarnessSupport.ReportShutdown"
            }
        }
    }

    fn description(&self) -> &'static str {
        match self {
            CliTelemetryEventDiscriminants::AgentRun => "Ran an agent from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentRunAmbient => {
                "Ran an ambient agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::AgentProfileList => {
                "Listed agent profiles from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::AgentList => "Listed agents from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentGet => "Got agent details from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentCreate => "Created an agent from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentUpdate => "Updated an agent from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentDelete => "Deleted an agent from the Derp CLI",
            CliTelemetryEventDiscriminants::AgentSkills => "Listed agent skills from the Derp CLI",
            CliTelemetryEventDiscriminants::EnvironmentList => {
                "Listed cloud environments from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::EnvironmentCreate => {
                "Created a cloud environment from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::EnvironmentDelete => {
                "Deleted a cloud environment from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::EnvironmentUpdate => {
                "Updated a cloud environment from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::EnvironmentGet => {
                "Got cloud environment details from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::EnvironmentImageList => {
                "Listed available base images from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MCPList => "Listed MCP servers from the Derp CLI",
            CliTelemetryEventDiscriminants::ModelList => "Listed models from the Derp CLI",
            CliTelemetryEventDiscriminants::MemoryStoreList => {
                "Listed memory stores from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreListMemories => {
                "Listed memories in a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreCreateMemory => {
                "Created a manual memory in a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreUpdateMemory => {
                "Updated a memory in a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreDeleteMemory => {
                "Deleted a memory from a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreGetStore => {
                "Got a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreUpdateStore => {
                "Updated a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreListStoreAgents => {
                "Listed agents attached to a memory store from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::MemoryStoreListVersions => {
                "Listed version history of a memory from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::TaskList => "Listed tasks from the Derp CLI",
            CliTelemetryEventDiscriminants::TaskGet => "Got status of task from the Derp CLI",
            CliTelemetryEventDiscriminants::ConversationGet => {
                "Got conversation by ID from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunConversationGet => {
                "Got run conversation from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunMessageWatch => {
                "Watched run messages from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunMessageSend => {
                "Sent a run message from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunMessageList => {
                "Listed run messages from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunMessageRead => {
                "Read a run message from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::RunMessageMarkDelivered => {
                "Marked a run message as delivered from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::Login => "Logged in via the Derp CLI",
            CliTelemetryEventDiscriminants::Logout => "Logged out via the Derp CLI",
            CliTelemetryEventDiscriminants::Whoami => "Printed current user info from the Derp CLI",
            CliTelemetryEventDiscriminants::ProviderSetup => "Set up a provider via the Derp CLI",
            CliTelemetryEventDiscriminants::ProviderList => "Listed providers from the Derp CLI",
            CliTelemetryEventDiscriminants::IntegrationCreate => {
                "Created an integration from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::IntegrationUpdate => {
                "Updated an integration from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::IntegrationList => {
                "Listed integrations from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ArtifactUpload => {
                "Uploaded an artifact from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ArtifactGet => {
                "Got artifact metadata from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ArtifactDownload => {
                "Downloaded an artifact from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ApiKeyList => "Listed API keys from the Derp CLI",
            CliTelemetryEventDiscriminants::ApiKeyCreate => "Created an API key from the Derp CLI",
            CliTelemetryEventDiscriminants::ApiKeyExpire => "Expired an API key from the Derp CLI",
            CliTelemetryEventDiscriminants::ScheduleCreate => {
                "Created a scheduled agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ScheduleList => {
                "Listed scheduled agents from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ScheduleGet => {
                "Got scheduled agent configuration from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::SchedulePause => {
                "Paused a scheduled agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ScheduleUnpause => {
                "Unpaused a scheduled agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ScheduleUpdate => {
                "Updated a scheduled agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::ScheduleDelete => {
                "Deleted a scheduled agent from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::SecretCreate => "Created a secret from the Derp CLI",
            CliTelemetryEventDiscriminants::SecretDelete => "Deleted a secret from the Derp CLI",
            CliTelemetryEventDiscriminants::SecretUpdate => "Updated a secret from the Derp CLI",
            CliTelemetryEventDiscriminants::SecretList => "Listed secrets from the Derp CLI",
            CliTelemetryEventDiscriminants::FederateIssueToken => {
                "Issued a federated identity token from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::FederateIssueGcpToken => {
                "Issued a GCP federated identity token from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::HarnessSupportPing => {
                "Pinged harness-support from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::HarnessSupportReportArtifact => {
                "Reported an artifact via harness-support from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::HarnessSupportNotifyUser => {
                "Sent a user notification via harness-support from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::HarnessSupportFinishTask => {
                "Reported task completion via harness-support from the Derp CLI"
            }
            CliTelemetryEventDiscriminants::HarnessSupportReportShutdown => {
                "Reported agent shutdown via harness-support from the Derp CLI"
            }
        }
    }

    fn enablement_state(&self) -> EnablementState {
        match self {
            Self::FederateIssueToken | Self::FederateIssueGcpToken => {
                EnablementState::Flag(FeatureFlag::OzIdentityFederation)
            }
            Self::HarnessSupportPing
            | Self::HarnessSupportReportArtifact
            | Self::HarnessSupportNotifyUser
            | Self::HarnessSupportFinishTask => EnablementState::Flag(FeatureFlag::AgentHarness),
            Self::ArtifactUpload | Self::ArtifactGet | Self::ArtifactDownload => {
                EnablementState::Flag(FeatureFlag::ArtifactCommand)
            }
            Self::ApiKeyList | Self::ApiKeyCreate | Self::ApiKeyExpire => {
                EnablementState::Flag(FeatureFlag::APIKeyManagement)
            }
            Self::RunMessageWatch
            | Self::RunMessageSend
            | Self::RunMessageList
            | Self::RunMessageRead
            | Self::RunMessageMarkDelivered => EnablementState::Always,
            _ => EnablementState::Always,
        }
    }
}

derp_core::register_telemetry_event!(CliTelemetryEvent);
