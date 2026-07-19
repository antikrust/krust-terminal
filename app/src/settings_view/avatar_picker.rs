//! The avatar picker on the Account settings page.
//!
//! Two rows of pictures: the art styles (one sample each) and, below them, the
//! variants of whichever style is currently yours. Clicking either writes
//! straight to [`IdentitySettings`], so the picker holds no state of its own
//! and always shows what is actually saved — there is no "unsaved selection"
//! to reconcile, and no Save button to forget to press.
//!
//! Only the current style's variants are rendered, which keeps this to ~28
//! images rather than the ~192 in the bundled set.
//!
//! The art is third-party and several styles are CC BY 4.0, so attribution is
//! a licence condition — it lives on the About page, sourced from
//! app/assets/bundled/avatars/LICENSES.md.

use warpui::assets::asset_cache::AssetSource;
use warpui::color::ColorU;
use warpui::elements::{
    Border, CacheOption, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Flex, Hoverable, Image, MouseStateHandle, ParentElement, Radius, Text, Wrap,
};
use warpui::platform::Cursor;
use warpui::AppContext;

use super::main_page::{MainPageAction, MainSettingsPageView};
use super::settings_page::SettingsWidget;
use crate::appearance::Appearance;
use crate::settings::identity::IdentitySettings;
use crate::ui_components::avatar_set;

/// Size of a picture in the variants row.
const VARIANT_SIZE: f32 = 56.;
/// Size of a picture in the styles row — smaller, since there are more of them.
const STYLE_SIZE: f32 = 40.;
const GAP: f32 = 8.;
/// The variant shown as a style's sample in the styles row.
const SAMPLE_SEED: &str = "01";

pub struct AvatarPickerWidget {
    /// One hover handle per style chip and per variant tile. Allocated once so
    /// the count is stable across renders, which is what these handles key on.
    style_states: Vec<MouseStateHandle>,
    variant_states: Vec<MouseStateHandle>,
}

impl Default for AvatarPickerWidget {
    fn default() -> Self {
        let max_variants = avatar_set::STYLES
            .iter()
            .map(|style| style.variants.len())
            .max()
            .unwrap_or_default();
        Self {
            style_states: (0..avatar_set::STYLES.len())
                .map(|_| MouseStateHandle::default())
                .collect(),
            variant_states: (0..max_variants)
                .map(|_| MouseStateHandle::default())
                .collect(),
        }
    }
}

impl AvatarPickerWidget {
    fn tile(
        path: &'static str,
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
                ConstrainedBox::new(
                    Image::new(AssetSource::Bundled { path }, CacheOption::BySize)
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
        "avatar picture profile image style"
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

        // An unknown style (older build, hand-edited settings) falls back to
        // the first bundled one so the picker still renders something usable.
        let style = avatar_set::style(&current_style)
            .or_else(|| avatar_set::STYLES.first())
            .expect("the bundled set is never empty");

        let mut styles_row = Wrap::row().with_run_spacing(GAP);
        for (index, candidate) in avatar_set::STYLES.iter().enumerate() {
            let Some(path) = avatar_set::asset_path(candidate.id, SAMPLE_SEED) else {
                continue;
            };
            let id = candidate.id;
            styles_row.add_child(
                Container::new(Self::tile(
                    path,
                    STYLE_SIZE,
                    candidate.id == style.id,
                    self.style_states[index].clone(),
                    appearance,
                    move |ctx| {
                        ctx.dispatch_typed_action(MainPageAction::SelectAvatarStyle(id.to_string()))
                    },
                ))
                .with_margin_right(GAP)
                .finish(),
            );
        }

        let mut variants_row = Wrap::row().with_run_spacing(GAP);
        for (index, (seed, path)) in style.variants.iter().enumerate() {
            let seed_owned = seed.to_string();
            variants_row.add_child(
                Container::new(Self::tile(
                    path,
                    VARIANT_SIZE,
                    *seed == current_seed && style.id == current_style,
                    self.variant_states[index].clone(),
                    appearance,
                    move |ctx| {
                        ctx.dispatch_typed_action(MainPageAction::SelectAvatarSeed(
                            seed_owned.clone(),
                        ))
                    },
                ))
                .with_margin_right(GAP)
                .finish(),
            );
        }

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(Self::label("Style", appearance))
                .with_child(
                    Container::new(styles_row.finish())
                        .with_margin_bottom(16.)
                        .finish(),
                )
                .with_child(Self::label(style.label, appearance))
                .with_child(variants_row.finish())
                .finish(),
        )
        .with_margin_top(8.)
        .with_margin_bottom(16.)
        .finish()
    }
}

/// Kept out of the widget so the About page can render the same credit
/// without depending on settings-page types.
///
/// This is a licence condition for the CC BY 4.0 styles, not a courtesy — see
/// app/assets/bundled/avatars/LICENSES.md for the per-style detail.
pub const AVATAR_ATTRIBUTION: &str =
    "Avatars generated with DiceBear (dicebear.com). Each style is © its respective artist, \
     licensed CC0, CC BY 4.0, or free for commercial use — see dicebear.com/licenses.";
