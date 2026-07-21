//! The avatar controls on the Account settings page.
//!
//! The current avatar is shown big in the account header (see
//! [`super::main_page`]); this widget is the options beneath it: **Upload a
//! photo**, and a DiceBear picker (art styles, then generated variants of the
//! chosen style). Every avatar is generated natively — see
//! [`crate::ui_components::dicebear`] — so the variants are live, not a frozen
//! bundle. Clicking writes straight to [`IdentitySettings`], so the widget holds
//! no state and always reflects what is saved.
//!
//! The art is third-party and several styles are CC BY 4.0, so attribution is a
//! licence condition — it lives on the About page (see [`AVATAR_ATTRIBUTION`]).

use warpui::assets::asset_cache::AssetSource;
use warpui::color::ColorU;
use warpui::elements::{
    Border, CacheOption, Container, CornerRadius, CrossAxisAlignment, Element, Flex, Hoverable,
    Image, MainAxisSize, MouseStateHandle, ParentElement, Radius, Text, Wrap,
};
use warpui::platform::Cursor;
use warpui::AppContext;

use super::main_page::{MainPageAction, MainSettingsPageView};
use super::settings_page::SettingsWidget;
use crate::appearance::Appearance;
use crate::settings::identity::IdentitySettings;
use crate::ui_components::dicebear;

/// Size of a picture in the variants row.
const VARIANT_SIZE: f32 = 56.;
/// Size of a picture in the styles row — smaller, since there are more of them.
const STYLE_SIZE: f32 = 40.;
const GAP: f32 = 8.;
/// The seed shown as a style's sample in the styles row.
const SAMPLE_SEED: &str = "1";
/// How many generated variants to offer for the chosen style.
const VARIANT_COUNT: usize = 15;

pub struct AvatarPickerWidget {
    /// One hover handle per style chip and per variant tile, plus the two action
    /// buttons. Allocated once so the count is stable across renders.
    style_states: Vec<MouseStateHandle>,
    variant_states: Vec<MouseStateHandle>,
    upload_state: MouseStateHandle,
    remove_state: MouseStateHandle,
}

impl Default for AvatarPickerWidget {
    fn default() -> Self {
        Self {
            style_states: (0..dicebear::styles().len())
                .map(|_| MouseStateHandle::default())
                .collect(),
            variant_states: (0..VARIANT_COUNT)
                .map(|_| MouseStateHandle::default())
                .collect(),
            upload_state: MouseStateHandle::default(),
            remove_state: MouseStateHandle::default(),
        }
    }
}

impl AvatarPickerWidget {
    /// A round avatar tile rendering a generated (or uploaded) source.
    fn tile(
        source: AssetSource,
        size: f32,
        selected: bool,
        state: MouseStateHandle,
        appearance: &Appearance,
        on_click: impl Fn(&mut warpui::EventContext) + 'static,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        Hoverable::new(state, move |mouse| {
            let border = if selected {
                Border::all(2.).with_border_color(theme.accent().into())
            } else if mouse.is_hovered() {
                Border::all(2.).with_border_color(theme.outline().into())
            } else {
                // Transparent rather than absent so tiles do not shift by two
                // pixels as selection and hover move between them.
                Border::all(2.).with_border_color(ColorU::transparent_black().into())
            };
            Container::new(
                warpui::elements::ConstrainedBox::new(
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
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| on_click(ctx))
        .finish()
    }

    /// A small text button (Upload / Remove).
    fn action_button(
        text: &str,
        state: MouseStateHandle,
        appearance: &Appearance,
        on_click: impl Fn(&mut warpui::EventContext) + 'static,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let label = text.to_string();
        Hoverable::new(state, move |mouse| {
            let bg = if mouse.is_hovered() {
                theme.surface_3()
            } else {
                theme.surface_2()
            };
            let text_fill = theme.main_text_color(bg.clone());
            Container::new(
                Text::new_inline(label.clone(), appearance.ui_font_family(), 13.)
                    .with_color(text_fill.into_solid())
                    .finish(),
            )
            .with_background(bg)
            .with_border(Border::all(1.).with_border_color(theme.outline().into()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .with_horizontal_padding(12.)
            .with_vertical_padding(6.)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| on_click(ctx))
        .finish()
    }

    fn label(text: &str, appearance: &Appearance) -> Box<dyn Element> {
        Container::new(
            Text::new_inline(text.to_string(), appearance.ui_font_family(), 12.)
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

impl SettingsWidget for AvatarPickerWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "avatar picture profile image style upload photo dicebear"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        use warpui::SingletonEntity as _;

        let settings = IdentitySettings::as_ref(app);
        let current_style = settings.avatar_style.to_string();
        let current_seed = settings.avatar_seed.to_string();
        let has_photo = !settings.avatar_photo_path.is_empty();

        // Upload a photo / remove it.
        let mut actions = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Self::action_button(
                "Upload a photo",
                self.upload_state.clone(),
                appearance,
                |ctx| ctx.dispatch_typed_action(MainPageAction::UploadAvatarPhoto),
            ));
        if has_photo {
            actions.add_child(
                Container::new(Self::action_button(
                    "Remove photo",
                    self.remove_state.clone(),
                    appearance,
                    |ctx| ctx.dispatch_typed_action(MainPageAction::RemoveAvatarPhoto),
                ))
                .with_margin_left(GAP)
                .finish(),
            );
        }

        // Art styles, one generated sample each.
        let mut styles_row = Wrap::row().with_run_spacing(GAP);
        for (index, style) in dicebear::styles().iter().enumerate() {
            let id = style.id.clone();
            let selected = !has_photo && style.id == current_style;
            styles_row.add_child(
                Container::new(Self::tile(
                    dicebear::avatar_source(&style.id, SAMPLE_SEED),
                    STYLE_SIZE,
                    selected,
                    self.style_states[index].clone(),
                    appearance,
                    move |ctx| {
                        ctx.dispatch_typed_action(MainPageAction::SelectAvatarStyle(id.clone()))
                    },
                ))
                .with_margin_right(GAP)
                .finish(),
            );
        }

        // Generated variants of the chosen style.
        let mut variants_row = Wrap::row().with_run_spacing(GAP);
        for index in 0..VARIANT_COUNT {
            let seed = (index + 1).to_string();
            let selected = !has_photo && seed == current_seed;
            variants_row.add_child(
                Container::new(Self::tile(
                    dicebear::avatar_source(&current_style, &seed),
                    VARIANT_SIZE,
                    selected,
                    self.variant_states[index].clone(),
                    appearance,
                    move |ctx| {
                        ctx.dispatch_typed_action(MainPageAction::SelectAvatarSeed(seed.clone()))
                    },
                ))
                .with_margin_right(GAP)
                .finish(),
            );
        }

        let style_label = dicebear::label(&current_style).unwrap_or("Avatar");

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(actions.finish())
                .with_child(
                    Container::new(Self::label("Choose an avatar", appearance))
                        .with_margin_top(16.)
                        .finish(),
                )
                .with_child(
                    Container::new(styles_row.finish())
                        .with_margin_bottom(16.)
                        .finish(),
                )
                .with_child(Self::label(style_label, appearance))
                .with_child(variants_row.finish())
                .finish(),
        )
        .with_margin_top(8.)
        .with_margin_bottom(16.)
        .finish()
    }
}

/// Kept out of the widget so the About page can render the same credit without
/// depending on settings-page types.
///
/// This is a licence condition for the CC BY 4.0 styles, not a courtesy — see
/// dicebear.com/licenses for the per-style detail.
pub const AVATAR_ATTRIBUTION: &str =
    "Avatars generated with DiceBear (dicebear.com). Each style is © its respective artist, \
     licensed CC0, CC BY 4.0, or free for commercial use — see dicebear.com/licenses.";
