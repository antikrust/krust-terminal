use derpui::elements::MouseStateHandle;
use derpui::{AppContext, Element};

use super::{DerpDriveItem, DerpDriveItemId};
use crate::appearance::Appearance;
use crate::cloud_object::CloudObjectMetadata;
use crate::drive::index::DriveIndexAction;
use crate::drive::DriveObjectType;
use crate::server::ids::ClientId;
use crate::themes::theme::Fill;

#[derive(Clone)]
pub struct DerpDriveAIFactCollection {
    id: ClientId,
}

impl DerpDriveAIFactCollection {
    pub fn new(id: ClientId) -> Self {
        Self { id }
    }

    pub fn id(&self) -> ClientId {
        self.id
    }
}

impl DerpDriveItem for DerpDriveAIFactCollection {
    fn display_name(&self) -> Option<String> {
        Some("Rules".to_string())
    }

    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        None
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        Some(DriveObjectType::AIFactCollection)
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        Some(DriveIndexAction::OpenAIFactCollection)
    }

    fn preview(&self, _appearance: &Appearance) -> Option<Box<dyn Element>> {
        None
    }

    fn derp_drive_id(&self) -> DerpDriveItemId {
        DerpDriveItemId::AIFactCollection
    }

    fn sync_status_icon(
        &self,
        _sync_queue_is_dequeueing: bool,
        _hover_state: MouseStateHandle,
        _appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        None
    }

    fn action_summary(&self, _app: &AppContext) -> Option<String> {
        None
    }

    fn clone_box(&self) -> Box<dyn DerpDriveItem> {
        Box::new(self.clone())
    }
}
