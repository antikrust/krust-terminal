//! The Moloch app: a window of its own for your Moloch server, in Krust's frame.
//!
//! The title bar is Krust's — the same height, the same close / minimise / fullscreen buttons on
//! the left, and your identity avatar on the right, so it is plain who is signed in — and below it a
//! left bar of Moloch's sections, each shown on its own (the pages the Moloch settings page shows
//! together). The window runs inside Krust, so it shares the sign-in, the avatar, the theme and
//! Krust's services rather than starting a second copy of them; the Moloch.app launcher opens it
//! with `krustoss://moloch`.

use std::sync::{Arc, Mutex, OnceLock};

use krustui::elements::{
    Align, Border, ChildAnchor, ChildView, ConstrainedBox, Container, CrossAxisAlignment,
    Expanded, Flex, MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Stack, Text,
};
use krustui::fonts::{Properties, Weight};
use krustui::geometry::vector::Vector2F;
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::windowing::WindowManager;
use krustui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
    WindowId,
};
use pathfinder_color::ColorU;

use crate::appearance::Appearance;
use crate::auth::auth_state::AuthState;
use crate::auth::AuthStateProvider;
use crate::root_view::default_window_options;
use crate::settings_view::{MolochPageView, MolochSection};
use crate::tab::TAB_BAR_BORDER_HEIGHT;
use crate::util::traffic_lights::{traffic_light_data, TrafficLightMouseStates};
use crate::window_settings::WindowSettings;
use crate::workspace::view::identity_avatar;
use crate::workspace::{TAB_BAR_HEIGHT, TOTAL_TAB_BAR_HEIGHT};

/// The left bar's width, as Krust's settings sidebar.
const LEFT_BAR_WIDTH: f32 = 200.;
/// Space above the first left-bar item, as the settings sidebar.
const LEFT_BAR_PADDING_TOP: f32 = 15.;
/// A left-bar item's indent, as a settings nav item.
const NAV_ITEM_MARGIN: f32 = 12.;
/// The title bar's bottom border, as the workspace's tab bar.
const TITLE_BAR_BORDER: ColorU = ColorU {
    r: 18,
    g: 18,
    b: 18,
    a: u8::MAX,
};

/// The Moloch window, while one is open.
fn open_window() -> &'static Mutex<Option<WindowId>> {
    static WINDOW: OnceLock<Mutex<Option<WindowId>>> = OnceLock::new();
    WINDOW.get_or_init(Default::default)
}

/// Opens the Moloch window, or brings it forward when one is already open.
pub fn open(ctx: &mut AppContext) {
    let existing = open_window().lock().ok().and_then(|window| *window);
    if let Some(window_id) = existing {
        if ctx.windows().platform_window(window_id).is_some() {
            ctx.windows().show_window_and_focus_app(window_id);
            return;
        }
    }
    let mut options = default_window_options(WindowSettings::as_ref(ctx), ctx);
    options.title = Some("Moloch".to_owned());
    let (window_id, _) = ctx.add_window(options, MolochWindow::new);
    if let Ok(mut window) = open_window().lock() {
        *window = Some(window_id);
    }
    // Krust draws the window buttons itself, in the title bar; the strip the OS drags the window
    // by (and double-clicks to zoom) is the title bar's height.
    if let Some(platform_window) = ctx.windows().platform_window(window_id) {
        #[cfg(target_os = "macos")]
        {
            use krustui::platform::mac::WindowExt;
            platform_window.as_ref().set_window_buttons(false);
        }
        let zoom = WindowSettings::as_ref(ctx).zoom_level.as_zoom_factor();
        platform_window
            .as_ref()
            .set_titlebar_height((TOTAL_TAB_BAR_HEIGHT * zoom) as f64);
    }
}

/// The window buttons in the title bar send the same actions as in a Krust window.
pub fn init(app: &mut AppContext) {
    app.add_action("root_view:close_window", MolochWindow::close_window);
    app.add_action("root_view:minimize_window", MolochWindow::minimize_window);
    app.add_action("root_view:toggle_fullscreen", MolochWindow::toggle_fullscreen);
}

pub struct MolochWindow {
    window_id: WindowId,
    auth_state: Arc<AuthState>,
    traffic_lights: TrafficLightMouseStates,
    /// One per section, in the left bar's order.
    nav_items: Vec<MouseStateHandle>,
    section: MolochSection,
    /// A page per section, each asking the server for what it shows.
    pages: Vec<(MolochSection, ViewHandle<MolochPageView>)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MolochWindowAction {
    Show(MolochSection),
}

impl MolochWindow {
    fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pages = MolochSection::ALL
            .iter()
            .map(|section| {
                let section = *section;
                let page =
                    ctx.add_typed_action_view(move |ctx| MolochPageView::new_section(section, ctx));
                (section, page)
            })
            .collect();
        Self {
            window_id: ctx.window_id(),
            auth_state: AuthStateProvider::as_ref(ctx).get().clone(),
            traffic_lights: TrafficLightMouseStates::default(),
            nav_items: MolochSection::ALL
                .iter()
                .map(|_| MouseStateHandle::default())
                .collect(),
            section: MolochSection::Overview,
            pages,
        }
    }

    fn close_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        if let Ok(mut window) = open_window().lock() {
            *window = None;
        }
        ctx.close_window();
        true
    }

    fn minimize_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.minimize_window();
        true
    }

    fn toggle_fullscreen(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        let window_id = ctx.window_id();
        WindowManager::handle(ctx).update(ctx, |state, ctx| {
            state.toggle_fullscreen(window_id, ctx);
        });
        true
    }

    /// Krust's title bar: black, the tab bar's height and bottom border, the window buttons where a
    /// Krust window has them, the window's name, and your avatar at the right.
    fn render_title_bar(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let theme = appearance.theme();
        let buttons = traffic_light_data(app, self.window_id);
        let buttons_width = buttons
            .as_ref()
            .map_or(0., |data| data.width(WindowSettings::as_ref(app).zoom_level.as_zoom_factor()));
        let (avatar, _) = identity_avatar(&self.auth_state, appearance, app);
        let contents = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Container::new(
                    Text::new_inline("Moloch", appearance.ui_font_family(), appearance.ui_font_size())
                        .with_color(theme.sub_text_color(theme.background()).into_solid())
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .finish(),
                )
                .with_margin_left(buttons_width + 8.)
                .finish(),
            )
            .with_child(Expanded::new(1., Flex::row().finish()).finish())
            .with_child(
                Container::new(avatar.build().finish())
                    .with_uniform_padding(2.)
                    .with_margin_right(8.)
                    .finish(),
            )
            .finish();
        let bar = Container::new(
            ConstrainedBox::new(Align::new(contents).finish())
                .with_height(TAB_BAR_HEIGHT)
                .finish(),
        )
        .with_border(Border::bottom(TAB_BAR_BORDER_HEIGHT).with_border_fill(TITLE_BAR_BORDER))
        .with_background_color(ColorU::black())
        .finish();

        let mut stack = Stack::new();
        stack.add_child(bar);
        if let Some(data) = buttons {
            let fullscreen_state = app
                .windows()
                .platform_window(self.window_id)
                .map(|window| window.fullscreen_state())
                .unwrap_or_default();
            stack.add_positioned_child(
                data.render(fullscreen_state, &self.traffic_lights, theme, app),
                OffsetPositioning::offset_from_parent(
                    Vector2F::zero(),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        ConstrainedBox::new(stack.finish())
            .with_height(TOTAL_TAB_BAR_HEIGHT)
            .finish()
    }

    /// The left bar: Moloch's sections, as Krust's settings sidebar lists its pages.
    fn render_left_bar(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut items = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (section, mouse) in MolochSection::ALL.iter().zip(&self.nav_items) {
            let section = *section;
            let variant = if section == self.section {
                ButtonVariant::Accent
            } else {
                ButtonVariant::Text
            };
            items.add_child(
                appearance
                    .ui_builder()
                    .button(variant, mouse.clone())
                    .with_text_label(section.label().to_string())
                    .with_style(
                        UiComponentStyles::default()
                            .set_border_width(0.)
                            .set_margin(
                                Coords::default()
                                    .left(NAV_ITEM_MARGIN)
                                    .right(NAV_ITEM_MARGIN),
                            )
                            .set_padding(Coords::uniform(8.)),
                    )
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(MolochWindowAction::Show(section));
                    })
                    .finish(),
            );
        }
        ConstrainedBox::new(
            Container::new(items.finish())
                .with_padding_top(LEFT_BAR_PADDING_TOP)
                .with_border(Border::right(1.).with_border_fill(theme.outline()))
                .finish(),
        )
        .with_width(LEFT_BAR_WIDTH)
        .finish()
    }
}

impl Entity for MolochWindow {
    type Event = ();
}

impl View for MolochWindow {
    fn ui_name() -> &'static str {
        "MolochWindow"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let page = self
            .pages
            .iter()
            .find(|(section, _)| *section == self.section)
            .map(|(_, page)| ChildView::new(page).finish());
        let mut body = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_left_bar(appearance));
        if let Some(page) = page {
            body.add_child(Expanded::new(1., page).finish());
        }
        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(self.render_title_bar(appearance, app))
                .with_child(Expanded::new(1., body.finish()).finish())
                .finish(),
        )
        .with_background(appearance.theme().background())
        .finish()
    }
}

impl TypedActionView for MolochWindow {
    type Action = MolochWindowAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MolochWindowAction::Show(section) => {
                self.section = *section;
                ctx.notify();
            }
        }
    }
}
