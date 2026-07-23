use derpui::elements::MouseStateHandle;
use derpui::Element;

use super::{DerpDriveItem, DerpDriveItemId};
use crate::appearance::Appearance;
use crate::cloud_object::{CloudObjectMetadata, Space};
use crate::drive::index::DriveIndexAction;
use crate::drive::DriveObjectType;
use crate::themes::theme::Fill;

#[derive(Clone)]
pub struct DerpDriveSpace {
    space: Space,
}

impl DerpDriveSpace {
    #[allow(dead_code)]
    pub fn new(space: Space) -> Self {
        Self { space }
    }
}

impl DerpDriveItem for DerpDriveSpace {
    fn display_name(&self) -> Option<String> {
        None
    }

    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        None
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        None
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        None
    }

    fn preview(&self, _appearance: &Appearance) -> Option<Box<dyn Element>> {
        None
    }

    fn derp_drive_id(&self) -> DerpDriveItemId {
        DerpDriveItemId::Space(self.space)
    }

    fn sync_status_icon(
        &self,
        _sync_queue_is_dequeueing: bool,
        _hover_state: MouseStateHandle,
        _appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        None
    }

    fn clone_box(&self) -> Box<dyn DerpDriveItem> {
        Box::new(self.clone())
    }

    fn action_summary(&self, _app: &derpui::AppContext) -> Option<String> {
        None
    }
}
