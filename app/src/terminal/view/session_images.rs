//! The right-hand panel listing the images of this terminal's agent session: what was attached
//! and what was generated, shown as pictures. The images are files; models hold only links to
//! them, so looking here costs a model nothing. Clicking one opens it in the system's viewer.

use ai::agent::ToolCallOutcome;
use krustui::elements::{
    Border, ConstrainedBox, Container, CrossAxisAlignment, Fill, Flex, Hoverable, Image,
    MouseStateHandle, NewScrollable, ParentElement, Text,
};
use krustui::assets::asset_cache::AssetSource;
use krustui::elements::new_scrollable::SingleAxisConfig;
use krustui::image_cache::CacheOption;
use krustui::platform::Cursor;
use krustui::{AppContext, Element, SingletonEntity};

use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;

const PANEL_WIDTH: f32 = 320.;
const IMAGE_HEIGHT: f32 = 180.;

/// One image of the session, with its mouse state (made here, never while rendering).
pub(super) struct SessionImage {
    pub(super) path: String,
    pub(super) mouse_state: MouseStateHandle,
}

impl SessionImage {
    pub(super) fn new(path: String) -> Self {
        Self {
            path,
            mouse_state: MouseStateHandle::default(),
        }
    }
}

/// The picture a `generate_image` call made, from its tool result: the tool answers with JSON
/// holding the `path` and a `markdown` link to it, either as the value itself or as text inside
/// `result`.
pub(super) fn generated_image_path(outcome: &ToolCallOutcome) -> Option<String> {
    let ToolCallOutcome::Success(value) = outcome else {
        return None;
    };
    let answer = match value.get("result").and_then(|result| result.as_str()) {
        Some(text) => serde_json::from_str::<serde_json::Value>(text).ok()?,
        None => value.clone(),
    };
    answer.get("markdown")?;
    let path = answer.get("path")?.as_str()?;
    let lower = path.to_ascii_lowercase();
    ["png", "jpg", "jpeg", "webp"]
        .iter()
        .any(|extension| lower.ends_with(&format!(".{extension}")))
        .then(|| path.to_owned())
}

impl TerminalView {
    /// Adds a picture to the session's list unless it is already there.
    pub(super) fn add_session_image(&mut self, path: String) {
        if !self.session_images.iter().any(|image| image.path == path) {
            self.session_images.push(SessionImage::new(path));
        }
    }

    pub(super) fn render_session_images_panel(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let main_text = blended_colors::text_main(theme, theme.background());
        let sub_text = blended_colors::text_sub(theme, theme.background());

        let mut column = Flex::column()
            .with_spacing(12.)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Text::new_inline(
                    "Images",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(main_text)
                .finish(),
            );
        if self.session_images.is_empty() {
            column.add_child(
                Text::new(
                    "Images you attach or generate in this session show here.",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(sub_text)
                .finish(),
            );
        }
        for image in &self.session_images {
            let source = AssetSource::LocalFile {
                path: image.path.clone(),
                content_version: None,
            };
            let path = image.path.clone();
            column.add_child(
                Hoverable::new(image.mouse_state.clone(), move |_| {
                    ConstrainedBox::new(Image::new(source.clone(), CacheOption::BySize).contain().finish())
                        .with_height(IMAGE_HEIGHT)
                        .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(TerminalAction::OpenSessionImage(path.clone()));
                })
                .finish(),
            );
        }

        let scrollable = NewScrollable::vertical(
            SingleAxisConfig::Clipped {
                handle: self.session_images_scroll_state.clone(),
                child: Container::new(column.finish())
                    .with_uniform_padding(12.)
                    .finish(),
            },
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .finish();

        ConstrainedBox::new(
            Container::new(scrollable)
                .with_background(theme.background())
                .with_border(Border::left(1.0).with_border_fill(theme.outline()))
                .finish(),
        )
        .with_width(PANEL_WIDTH)
        .finish()
    }
}
