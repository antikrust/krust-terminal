use derpui::elements::{Flex, MouseStateHandle, ParentElement};
use derpui::fonts::Weight;
use derpui::ui_components::components::{UiComponent, UiComponentStyles};
use derpui::{AppContext, Element};

use super::{DerpDriveItem, DerpDriveItemId};
use crate::appearance::Appearance;
use crate::cloud_object::CloudObjectMetadata;
use crate::drive::index::DriveIndexAction;
use crate::drive::{CloudObjectTypeAndId, DriveObjectType};
use crate::notebooks::CloudNotebook;
use crate::themes::theme::Fill;

#[derive(Clone)]
pub struct DerpDriveNotebook {
    id: CloudObjectTypeAndId,
    notebook: CloudNotebook,
    is_ai_document: bool,
}

impl DerpDriveNotebook {
    pub fn new(id: CloudObjectTypeAndId, notebook: CloudNotebook, is_ai_document: bool) -> Self {
        Self {
            id,
            notebook,
            is_ai_document,
        }
    }
}

impl DerpDriveItem for DerpDriveNotebook {
    fn display_name(&self) -> Option<String> {
        if self.notebook.model().title.is_empty() {
            None
        } else {
            Some(self.notebook.model().title.clone())
        }
    }

    fn metadata(&self) -> Option<&CloudObjectMetadata> {
        Some(&self.notebook.metadata)
    }

    fn object_type(&self) -> Option<DriveObjectType> {
        Some(DriveObjectType::Notebook {
            is_ai_document: self.is_ai_document,
        })
    }

    fn secondary_icon(&self, _color: Option<Fill>) -> Option<Box<dyn Element>> {
        None
    }

    fn click_action(&self) -> Option<DriveIndexAction> {
        Some(DriveIndexAction::OpenObject(self.id))
    }

    fn preview(&self, appearance: &Appearance) -> Option<Box<dyn Element>> {
        let title_text = self.notebook.model().title.clone();
        let title_to_render = if title_text.is_empty() {
            "Untitled".to_string()
        } else {
            title_text
        };
        let title = appearance
            .ui_builder()
            .wrappable_text(title_to_render, true)
            .with_style(UiComponentStyles {
                font_color: Some(
                    appearance
                        .theme()
                        .main_text_color(appearance.theme().background())
                        .into(),
                ),
                font_size: Some(14.),
                font_weight: Some(Weight::Bold),
                ..Default::default()
            })
            .build()
            .finish();

        Some(Flex::column().with_child(title).finish())
    }

    fn derp_drive_id(&self) -> DerpDriveItemId {
        DerpDriveItemId::Object(self.id)
    }

    fn sync_status_icon(
        &self,
        sync_queue_is_dequeueing: bool,
        hover_state: MouseStateHandle,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        self.notebook.metadata.pending_changes_statuses.render_icon(
            sync_queue_is_dequeueing,
            hover_state,
            appearance,
        )
    }

    fn action_summary(&self, _app: &AppContext) -> Option<String> {
        None
    }

    fn clone_box(&self) -> Box<dyn DerpDriveItem> {
        Box::new(self.clone())
    }
}
