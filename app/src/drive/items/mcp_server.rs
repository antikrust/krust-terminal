use krustui::elements::MouseStateHandle;
use krustui::{AppContext, Element};

use super::{KrustDriveItem, KrustDriveItemId};
use crate::ai::mcp::CloudMCPServer;
use crate::appearance::Appearance;
use crate::cloud_object::CloudObjectMetadata;
use crate::drive::index::DriveIndexAction;
use crate::drive::{CloudObjectTypeAndId, DriveObjectType};
use crate::themes::theme::Fill;

#[derive(Clone)]
pub struct KrustDriveMCPServer {
    id: CloudObjectTypeAndId,
    mcp_server: CloudMCPServer,
}

impl KrustDriveMCPServer {
    pub fn new(id: CloudObjectTypeAndId, mcp_server: CloudMCPServer) -> Self {
        Self { id, mcp_server }
    }
}

impl KrustDriveItem for KrustDriveMCPServer {
    fn display_name(&self) -> Option<String> {
        Some(self.mcp_server.model().string_model.name.clone())
    }
    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        Some(&self.mcp_server.metadata)
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        Some(DriveObjectType::MCPServer)
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        Some(DriveIndexAction::OpenMCPServerCollection)
    }

    fn preview(&self, _appearance: &Appearance) -> Option<Box<dyn Element>> {
        // TODO
        None
    }

    fn krust_drive_id(&self) -> KrustDriveItemId {
        KrustDriveItemId::Object(self.id)
    }

    fn sync_status_icon(
        &self,
        sync_queue_is_dequeueing: bool,
        hover_state: MouseStateHandle,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        self.mcp_server
            .metadata
            .pending_changes_statuses
            .render_icon(sync_queue_is_dequeueing, hover_state, appearance)
    }

    fn action_summary(&self, _app: &AppContext) -> Option<String> {
        None
    }

    fn clone_box(&self) -> Box<dyn KrustDriveItem> {
        Box::new(self.clone())
    }
}
