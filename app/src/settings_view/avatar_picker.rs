//! The avatar picker modal.
//!
//! The Account page shows exactly one avatar (see [`super::main_page`]);
//! clicking it opens this modal: your current pick at the top (next to the
//! Upload / Reshuffle controls), the "Saved" shelf of pinned candidates under
//! it, then named sections down the left (one per DiceBear art style) and a
//! scrollable grid of generated variants. Reshuffle deals the whole grid
//! again with fresh random seeds; the pin chip on each tile puts a face aside
//! on the shelf before it is shuffled away. Clicking any tile makes it your
//! avatar immediately; a photo can be uploaded instead, which wins over any
//! generated pick.
//!
//! Every picture is generated natively on demand — see
//! [`crate::ui_components::dicebear`] — so the grid is live art, not a frozen
//! bundle. All state lives in [`IdentitySettings`] (selection, photo,
//! favorites), so the modal reflects what is saved and nothing more; the only
//! view-local state is which section you are browsing.
//!
//! The art is third-party and several styles are CC BY 4.0, so attribution is a
//! licence condition — it lives on the About page (see [`AVATAR_ATTRIBUTION`]).

use ::settings::Setting as _;
use warp_core::ui::icons::Icon;
use warp_errors::report_if_error;
use warpui::color::ColorU;
use warpui::elements::{
    self, Border, CacheOption, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CornerRadius, CrossAxisAlignment, Element, Fill, Flex, Hoverable, Image,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius, ScrollbarWidth,
    Stack, Text, Wrap,
};
use warpui::platform::file_picker::FilePickerConfiguration;
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::units::Pixels;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::appearance::Appearance;
use crate::settings::identity::{IdentitySettings, MAX_AVATAR_FAVORITES};
use crate::ui_components::dicebear;

/// Width of the whole modal; the grid and rail sizes below are derived to fit.
pub const AVATAR_MODAL_WIDTH: f32 = 680.;
const RAIL_WIDTH: f32 = 132.;
const CONTENT_GAP: f32 = 16.;
/// 24px modal padding on each side.
const CONTENT_WIDTH: f32 = AVATAR_MODAL_WIDTH - 48. - RAIL_WIDTH - CONTENT_GAP;
const GRID_HEIGHT: f32 = 316.;
const TILE_SIZE: f32 = 60.;
/// The current pick shown at the top of the modal.
const CURRENT_SIZE: f32 = 60.;
const SAVED_TILE_SIZE: f32 = 44.;
const TILE_GAP: f32 = 8.;
/// How many generated variants each section offers. Seeds are just "1"..N —
/// generation is deterministic, so the same grid comes back every time.
const VARIANT_COUNT: usize = dicebear::VARIANT_SEEDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarPickerAction {
    /// Browse a different style section (does not change the chosen avatar).
    SelectSection(String),
    /// Make `(style, seed)` the avatar.
    Select { style: String, seed: String },
    /// Pin `(style, seed)` to the Saved shelf, or unpin it.
    ToggleFavorite { style: String, seed: String },
    /// Deal a fresh set of faces: every grid tile gets a new random seed.
    /// Pin anything you like first — a reshuffle replaces the whole grid.
    Reshuffle,
    /// Pick a local photo to use instead of a generated avatar.
    UploadPhoto,
    /// Drop the uploaded photo, reverting to the generated avatar.
    RemovePhoto,
}

pub struct AvatarPickerModal {
    /// The section being browsed. View-local: browsing is not choosing.
    selected_style: String,
    /// The seeds behind the grid tiles. Starts as "1".."48" (the range starter
    /// avatars are assigned from) and is replaced wholesale by Reshuffle.
    /// Session-local on purpose: the Saved shelf is the durable place.
    variant_seeds: Vec<String>,
    scroll_state: ClippedScrollStateHandle,
    section_states: Vec<MouseStateHandle>,
    tile_states: Vec<MouseStateHandle>,
    tile_pin_states: Vec<MouseStateHandle>,
    saved_states: Vec<MouseStateHandle>,
    saved_pin_states: Vec<MouseStateHandle>,
    upload_state: MouseStateHandle,
    reshuffle_state: MouseStateHandle,
    remove_state: MouseStateHandle,
}

impl AvatarPickerModal {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        // Favorites and the chosen avatar can change from outside (settings
        // sync); keep the modal honest.
        ctx.subscribe_to_model(&IdentitySettings::handle(ctx), |_, _, _, ctx| ctx.notify());
        Self {
            selected_style: dicebear::DEFAULT_STYLE.to_owned(),
            variant_seeds: (1..=VARIANT_COUNT).map(|n| n.to_string()).collect(),
            scroll_state: Default::default(),
            section_states: (0..dicebear::styles().len())
                .map(|_| MouseStateHandle::default())
                .collect(),
            tile_states: (0..VARIANT_COUNT)
                .map(|_| MouseStateHandle::default())
                .collect(),
            tile_pin_states: (0..VARIANT_COUNT)
                .map(|_| MouseStateHandle::default())
                .collect(),
            saved_states: (0..MAX_AVATAR_FAVORITES)
                .map(|_| MouseStateHandle::default())
                .collect(),
            saved_pin_states: (0..MAX_AVATAR_FAVORITES)
                .map(|_| MouseStateHandle::default())
                .collect(),
            upload_state: MouseStateHandle::default(),
            reshuffle_state: MouseStateHandle::default(),
            remove_state: MouseStateHandle::default(),
        }
    }

    /// Called when the modal opens: land on the section of the current avatar.
    pub fn sync_to_current_choice(&mut self, ctx: &mut ViewContext<Self>) {
        let style = IdentitySettings::as_ref(ctx).avatar_style.to_string();
        if dicebear::label(&style).is_some() {
            self.selected_style = style;
        }
        self.scroll_state.scroll_to(Pixels::zero());
        ctx.notify();
    }

    /// One avatar tile: the generated picture, a selection/hover ring, and a
    /// pin chip (shown on hover, or always when pinned) for the Saved shelf.
    /// The chip is a child click target; the tile defers to it.
    #[allow(clippy::too_many_arguments)]
    fn tile(
        style: &str,
        seed: &str,
        size: f32,
        selected: bool,
        favorited: bool,
        state: MouseStateHandle,
        pin_state: MouseStateHandle,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let accent = theme.accent();
        let outline = theme.outline();
        let chip_bg = theme.surface_1();
        let chip_fg = theme.main_text_color(theme.surface_1());
        let source = dicebear::avatar_source(style, seed);
        let select = AvatarPickerAction::Select {
            style: style.to_owned(),
            seed: seed.to_owned(),
        };
        let pin = AvatarPickerAction::ToggleFavorite {
            style: style.to_owned(),
            seed: seed.to_owned(),
        };
        Hoverable::new(state, move |mouse| {
            let border = if selected {
                Border::all(2.).with_border_fill(accent.clone())
            } else if mouse.is_hovered() {
                Border::all(2.).with_border_fill(outline.clone())
            } else {
                // Transparent rather than absent so tiles do not shift by two
                // pixels as selection and hover move between them.
                Border::all(2.).with_border_color(ColorU::transparent_black())
            };
            let picture = Container::new(
                ConstrainedBox::new(
                    Image::new(source.clone(), CacheOption::BySize)
                        .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                        .finish(),
                )
                .with_width(size)
                .with_height(size)
                .finish(),
            )
            .with_border(border)
            .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
            .with_uniform_padding(2.)
            .finish();

            let mut stack = Stack::new().with_child(picture);
            if mouse.is_hovered() || favorited {
                let pin = pin.clone();
                let accent = accent.clone();
                let chip_bg = chip_bg.clone();
                let chip_fg = chip_fg.clone();
                let chip = Hoverable::new(pin_state.clone(), move |_| {
                    let (bg, fg): (Fill, ColorU) = if favorited {
                        (accent.clone().into(), ColorU::white())
                    } else {
                        (chip_bg.clone().into(), chip_fg.clone().into())
                    };
                    Container::new(
                        ConstrainedBox::new(elements::Icon::new(Icon::Pin.into(), fg).finish())
                            .with_width(10.)
                            .with_height(10.)
                            .finish(),
                    )
                    .with_background(bg)
                    .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                    .with_uniform_padding(3.)
                    .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(pin.clone()))
                .finish();
                // The overlay must carry its own finite size: the wrap grid
                // measures tiles under an infinite max constraint, where an
                // unconstrained expanding flex is a layout error (a debug-build
                // panic — the modal crashed whenever a chip was visible).
                // Tile box = picture + 2px padding and 2px border per side.
                let tile_box = size + 8.;
                stack = stack.with_child(
                    ConstrainedBox::new(
                        Flex::row()
                            .with_main_axis_size(MainAxisSize::Max)
                            .with_main_axis_alignment(MainAxisAlignment::End)
                            .with_cross_axis_alignment(CrossAxisAlignment::Start)
                            .with_child(chip)
                            .finish(),
                    )
                    .with_width(tile_box)
                    .with_height(tile_box)
                    .finish(),
                );
            }
            stack.finish()
        })
        .with_defer_events_to_children()
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(select.clone()))
        .finish()
    }

    fn section_row(
        &self,
        index: usize,
        style: &dicebear::StyleInfo,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let selected = style.id == self.selected_style;
        let label = style.label.clone();
        let font = appearance.ui_font_family();
        let text_color = if selected {
            theme.active_ui_text_color()
        } else {
            theme.nonactive_ui_text_color()
        };
        let hover_bg = theme.surface_3();
        let action = AvatarPickerAction::SelectSection(style.id.clone());
        Hoverable::new(self.section_states[index].clone(), move |mouse| {
            let bg: Fill = if selected || mouse.is_hovered() {
                hover_bg.clone().into()
            } else {
                Fill::None
            };
            Container::new(
                Text::new_inline(label.clone(), font, 13.)
                    .with_color(text_color.clone().into_solid())
                    .finish(),
            )
            .with_background(bg)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(5.)))
            .with_horizontal_padding(10.)
            .with_vertical_padding(5.)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    fn small_label(text: &str, appearance: &Appearance) -> Box<dyn Element> {
        Container::new(
            Text::new_inline(text.to_string(), appearance.ui_font_family(), 11.)
                .with_color(
                    appearance
                        .theme()
                        .sub_text_color(appearance.theme().surface_2())
                        .into_solid(),
                )
                .finish(),
        )
        .with_margin_bottom(6.)
        .finish()
    }
}

impl Entity for AvatarPickerModal {
    type Event = ();
}

impl View for AvatarPickerModal {
    fn ui_name() -> &'static str {
        "AvatarPickerModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let settings = IdentitySettings::as_ref(app);
        let chosen_style = settings.avatar_style.to_string();
        let chosen_seed = settings.avatar_seed.to_string();
        let has_photo = !settings.avatar_photo_path.is_empty();
        let favorites = IdentitySettings::favorite_avatars(app);

        // Left rail: one named section per art style.
        let mut rail = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (index, style) in dicebear::styles().iter().enumerate() {
            rail.add_child(
                Container::new(self.section_row(index, style, appearance))
                    .with_margin_bottom(2.)
                    .finish(),
            );
        }

        // Controls: upload always, reshuffle always; remove only with a photo.
        let button_style = UiComponentStyles {
            font_size: Some(13.),
            padding: Some(Coords::uniform(6.).left(12.).right(12.)),
            ..Default::default()
        };
        let mut controls = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                appearance
                    .ui_builder()
                    .button(ButtonVariant::Outlined, self.upload_state.clone())
                    .with_text_label("Upload a photo".to_string())
                    .with_style(button_style.clone())
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(AvatarPickerAction::UploadPhoto)
                    })
                    .finish(),
            )
            .with_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .button(ButtonVariant::Secondary, self.reshuffle_state.clone())
                        .with_text_label("Reshuffle".to_string())
                        .with_style(button_style.clone())
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(AvatarPickerAction::Reshuffle)
                        })
                        .finish(),
                )
                .with_margin_left(TILE_GAP)
                .finish(),
            );
        if has_photo {
            controls.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .button(ButtonVariant::Secondary, self.remove_state.clone())
                        .with_text_label("Remove photo".to_string())
                        .with_style(button_style)
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(AvatarPickerAction::RemovePhoto)
                        })
                        .finish(),
                )
                .with_margin_left(TILE_GAP)
                .finish(),
            );
        }

        let mut content = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Start);

        // The current pick, big and first, with the controls beside it.
        let current_picture: Box<dyn Element> = {
            let theme = appearance.theme();
            match IdentitySettings::selected_avatar_source(app) {
                Some(source) => Container::new(
                    ConstrainedBox::new(
                        Image::new(source, CacheOption::BySize)
                            .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                            .finish(),
                    )
                    .with_width(CURRENT_SIZE)
                    .with_height(CURRENT_SIZE)
                    .finish(),
                )
                .with_border(Border::all(2.).with_border_fill(theme.accent()))
                .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                .with_uniform_padding(2.)
                .finish(),
                None => Container::new(
                    ConstrainedBox::new(elements::Empty::new().finish())
                        .with_width(CURRENT_SIZE)
                        .with_height(CURRENT_SIZE)
                        .finish(),
                )
                .with_background(theme.surface_3())
                .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                .with_uniform_padding(2.)
                .finish(),
            }
        };
        content.add_child(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Min)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        Container::new(current_picture)
                            .with_margin_right(CONTENT_GAP)
                            .finish(),
                    )
                    .with_child(controls.finish())
                    .finish(),
            )
            .with_margin_bottom(14.)
            .finish(),
        );

        // The Saved shelf: everything pinned, across all sections.
        if !favorites.is_empty() {
            content.add_child(Self::small_label("Saved", appearance));
            let mut shelf = Wrap::row().with_run_spacing(TILE_GAP);
            for (index, (style, seed)) in favorites.iter().take(MAX_AVATAR_FAVORITES).enumerate() {
                let selected = !has_photo && *style == chosen_style && *seed == chosen_seed;
                shelf.add_child(
                    Container::new(Self::tile(
                        style,
                        seed,
                        SAVED_TILE_SIZE,
                        selected,
                        true,
                        self.saved_states[index].clone(),
                        self.saved_pin_states[index].clone(),
                        appearance,
                    ))
                    .with_margin_right(TILE_GAP)
                    .finish(),
                );
            }
            // Like the grid below, the shelf wrap needs a finite width: the
            // content column measures children under an infinite max.
            content.add_child(
                ConstrainedBox::new(
                    Container::new(shelf.finish())
                        .with_margin_bottom(12.)
                        .finish(),
                )
                .with_width(CONTENT_WIDTH)
                .finish(),
            );
        }

        // The variant grid for the browsed section.
        let section_label = dicebear::label(&self.selected_style).unwrap_or("Avatars");
        content.add_child(Self::small_label(section_label, appearance));
        let mut grid = Wrap::row().with_run_spacing(TILE_GAP);
        for (index, seed) in self.variant_seeds.iter().take(VARIANT_COUNT).enumerate() {
            let selected =
                !has_photo && self.selected_style == chosen_style && *seed == chosen_seed;
            let favorited = favorites
                .iter()
                .any(|(s, d)| *s == self.selected_style && d == seed);
            grid.add_child(
                Container::new(Self::tile(
                    &self.selected_style,
                    seed,
                    TILE_SIZE,
                    selected,
                    favorited,
                    self.tile_states[index].clone(),
                    self.tile_pin_states[index].clone(),
                    appearance,
                ))
                .with_margin_right(TILE_GAP)
                .finish(),
            );
        }
        content.add_child(
            ConstrainedBox::new(
                ClippedScrollable::vertical(
                    self.scroll_state.clone(),
                    Container::new(grid.finish())
                        .with_padding_bottom(TILE_GAP)
                        .finish(),
                    ScrollbarWidth::Auto,
                    theme.disabled_text_color(theme.surface_2()).into(),
                    theme.main_text_color(theme.surface_2()).into(),
                    Fill::None,
                )
                .finish(),
            )
            .with_width(CONTENT_WIDTH)
            .with_height(GRID_HEIGHT)
            .finish(),
        );

        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                ConstrainedBox::new(rail.finish())
                    .with_width(RAIL_WIDTH)
                    .finish(),
            )
            .with_child(
                Container::new(content.finish())
                    .with_margin_left(CONTENT_GAP)
                    .finish(),
            )
            .finish()
    }
}

impl TypedActionView for AvatarPickerModal {
    type Action = AvatarPickerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AvatarPickerAction::SelectSection(style) => {
                self.selected_style = style.clone();
                self.scroll_state.scroll_to(Pixels::zero());
                ctx.notify();
            }
            AvatarPickerAction::Select { style, seed } => {
                IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.avatar_style.set_value(style.clone(), ctx));
                    report_if_error!(settings.avatar_seed.set_value(seed.clone(), ctx));
                    // Choosing a generated avatar clears any uploaded photo.
                    report_if_error!(settings.avatar_photo_path.set_value(String::new(), ctx));
                });
                ctx.notify();
            }
            AvatarPickerAction::ToggleFavorite { style, seed } => {
                IdentitySettings::toggle_favorite_avatar(style, seed, ctx);
                ctx.notify();
            }
            AvatarPickerAction::Reshuffle => {
                use rand::Rng as _;
                let mut rng = rand::thread_rng();
                let mut seen = std::collections::HashSet::new();
                self.variant_seeds = std::iter::from_fn(|| Some(rng.gen_range(1..=999_999u32)))
                    .filter(|n| seen.insert(*n))
                    .take(VARIANT_COUNT)
                    .map(|n| n.to_string())
                    .collect();
                self.scroll_state.scroll_to(Pixels::zero());
                ctx.notify();
            }
            AvatarPickerAction::UploadPhoto => {
                ctx.open_file_picker(
                    move |result, ctx| {
                        if let Some(Ok(path)) =
                            result.map(|paths| paths.into_iter().next()).transpose()
                        {
                            IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                                report_if_error!(settings.avatar_photo_path.set_value(path, ctx));
                            });
                            ctx.notify();
                        }
                    },
                    FilePickerConfiguration::new(),
                );
            }
            AvatarPickerAction::RemovePhoto => {
                IdentitySettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.avatar_photo_path.set_value(String::new(), ctx));
                });
                ctx.notify();
            }
        }
    }
}

#[cfg(test)]
#[path = "avatar_picker_tests.rs"]
mod tests;

/// Kept out of the modal so the About page can render the same credit without
/// depending on settings-page types.
///
/// This is a licence condition for the CC BY 4.0 styles, not a courtesy — see
/// dicebear.com/licenses for the per-style detail.
pub const AVATAR_ATTRIBUTION: &str =
    "Avatars generated with DiceBear (dicebear.com). Each style is © its respective artist, \
     licensed CC0, CC BY 4.0, or free for commercial use — see dicebear.com/licenses.";
