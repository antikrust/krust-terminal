//! The right-hand panel listing the images of this terminal's agent session: what was attached
//! and what was generated, shown as pictures. The images are files; models hold only links to
//! them, so looking here costs a model nothing. Clicking one opens it in the system's viewer.

use ai::agent::ToolCallOutcome;
use krustui::elements::{
    Border, ConstrainedBox, Container, CrossAxisAlignment, Fill, Flex, Hoverable, Image,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, NewScrollable, ParentElement, Text,
};
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::assets::asset_cache::AssetSource;
use krustui::elements::new_scrollable::SingleAxisConfig;
use krustui::image_cache::CacheOption;
use krustui::platform::Cursor;
use krustui::{AppContext, Element, SingletonEntity};

use super::{TerminalAction, TerminalView};
use crate::appearance::Appearance;
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button;
use crate::ui_components::icons::Icon;

const PANEL_WIDTH: f32 = 320.;
const PANEL_PADDING: f32 = 12.;
const GRID_SPACING: f32 = 8.;
const SLIDER_WIDTH: f32 = 110.;
/// The most pictures the panel shows side by side.
pub(super) const MAX_PER_ROW: usize = 4;

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

        let per_row = self.session_images_per_row.clamp(1, MAX_PER_ROW);
        // Pictures are squares fitted into equal cells across the panel's inner width.
        let inner_width = PANEL_WIDTH - 2. * PANEL_PADDING;
        let cell = (inner_width - GRID_SPACING * (per_row as f32 - 1.)) / per_row as f32;

        let set_per_row = |ctx: &mut krustui::EventContext, value: f32| {
            ctx.dispatch_typed_action(TerminalAction::SetSessionImagesPerRow(value.round() as usize));
        };
        let slider = appearance
            .ui_builder()
            .slider(self.session_images_slider_state.clone())
            .with_range(1.0..MAX_PER_ROW as f32)
            .with_step(1.)
            .with_default_value(per_row as f32)
            .with_style(UiComponentStyles {
                width: Some(SLIDER_WIDTH),
                margin: Some(Coords::default().left(8.).right(8.)),
                ..Default::default()
            })
            .on_drag(move |ctx, _, value| set_per_row(ctx, value))
            .on_change(move |ctx, _, value| set_per_row(ctx, value))
            .build()
            .finish();
        // The same close button as the conversation details panel's, at the right of the header.
        let close_button = icon_button(
            appearance,
            Icon::X,
            false,
            self.session_images_close_mouse_state.clone(),
        )
        .build()
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(TerminalAction::ToggleSessionImagesPanel);
        })
        .with_cursor(Cursor::PointingHand)
        .finish();
        let title_and_slider = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new_inline(
                    "Images",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(main_text)
                .finish(),
            )
            .with_child(slider)
            .with_child(
                Text::new_inline(
                    format!("{per_row} per row"),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(sub_text)
                .finish(),
            )
            .finish();
        let header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(title_and_slider)
            .with_child(close_button)
            .finish();

        let mut column = Flex::column()
            .with_spacing(12.)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header);
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
        for row_images in self.session_images.chunks(per_row) {
            let mut row = Flex::row()
                .with_spacing(GRID_SPACING)
                .with_cross_axis_alignment(CrossAxisAlignment::Start);
            for image in row_images {
                let source = AssetSource::LocalFile {
                    path: image.path.clone(),
                    content_version: None,
                };
                let path = image.path.clone();
                row.add_child(
                    Hoverable::new(image.mouse_state.clone(), move |_| {
                        ConstrainedBox::new(
                            Image::new(source.clone(), CacheOption::BySize).contain().finish(),
                        )
                        .with_width(cell)
                        .with_height(cell)
                        .finish()
                    })
                    .with_cursor(Cursor::PointingHand)
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(TerminalAction::OpenSessionImage(path.clone()));
                    })
                    .finish(),
                );
            }
            column.add_child(row.finish());
        }

        let scrollable = NewScrollable::vertical(
            SingleAxisConfig::Clipped {
                handle: self.session_images_scroll_state.clone(),
                child: Container::new(column.finish())
                    .with_uniform_padding(PANEL_PADDING)
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
