//! The window, in Krust's frame. The title bar is the workspace's tab bar — black, 28px plus a 1px
//! border of rgb(18,18,18) — with Krust's macOS window buttons (the "Bob" controls of
//! `app/src/util/traffic_lights.rs`: close, minimise, fullscreen, on the left), the app's name, and
//! your identity avatar on the right. Below it, a 200px left bar in the settings sidebar's style
//! (1px outline border, items padded 8px, the selected one in the accent), and the page it picks.

use krust_core::ui::appearance::Appearance;
use krustui::elements::{
    Align, Border, ChildAnchor, ChildView, ConstrainedBox, Container, CrossAxisAlignment, Element,
    Expanded, Flex, Hoverable, Icon, MouseStateHandle, OffsetPositioning, ParentAnchor,
    ParentElement, ParentOffsetBounds, Rect, Stack, Text,
};
use krustui::fonts::{Properties, Weight};
use krustui::geometry::vector::Vector2F;
use krustui::platform::TerminationMode;
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::windowing::WindowManager;
use krustui::{
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle, WindowId,
};
use pathfinder_color::ColorU;

use crate::avatar;

/// The tab bar's height, and its bottom border.
const TITLE_BAR_HEIGHT: f32 = 28.;
const TITLE_BAR_BORDER: f32 = 1.;
const TITLE_BAR_BORDER_COLOR: ColorU = ColorU {
    r: 18,
    g: 18,
    b: 18,
    a: u8::MAX,
};
/// Krust's window buttons: 22px hit targets, 7px in from the left.
const BUTTON_SIZE: f32 = 22.;
const BUTTONS_INSET: f32 = 7.;
const BUTTONS_WIDTH: f32 = BUTTONS_INSET + BUTTON_SIZE * 3.;
const GREEN: ColorU = ColorU {
    r: 95,
    g: 224,
    b: 106,
    a: 224,
};
const AMBER: ColorU = ColorU {
    r: 255,
    g: 178,
    b: 74,
    a: 224,
};
const RED: ColorU = ColorU {
    r: 255,
    g: 107,
    b: 102,
    a: 224,
};
/// The left bar, as Krust's settings sidebar.
const LEFT_BAR_WIDTH: f32 = 200.;
const LEFT_BAR_PADDING_TOP: f32 = 15.;
const NAV_ITEM_MARGIN: f32 = 12.;

/// One page of the app: its name in the left bar, and how to draw it.
pub struct Page {
    pub label: &'static str,
    render: Box<dyn Fn() -> Box<dyn Element>>,
}

impl Page {
    /// A page that is a view of the app's.
    pub fn view<T: View>(label: &'static str, handle: ViewHandle<T>) -> Self {
        Self {
            label,
            render: Box::new(move || ChildView::new(&handle).finish()),
        }
    }
}

/// The window buttons send these, as a Krust window's send `root_view:*`.
pub fn init(app: &mut AppContext) {
    app.add_action("shell:close_window", ShellWindow::close_window);
    app.add_action("shell:minimize_window", ShellWindow::minimize_window);
    app.add_action("shell:toggle_fullscreen", ShellWindow::toggle_fullscreen);
}

/// Krust draws the window buttons itself, in the title bar; the strip the OS drags the window by
/// (and double-clicks to zoom) is the title bar's height.
pub fn style_window(window_id: WindowId, ctx: &mut AppContext) {
    if let Some(window) = ctx.windows().platform_window(window_id) {
        #[cfg(target_os = "macos")]
        {
            use krustui::platform::mac::WindowExt;
            window.as_ref().set_window_buttons(false);
        }
        window
            .as_ref()
            .set_titlebar_height((TITLE_BAR_HEIGHT + TITLE_BAR_BORDER) as f64);
    }
}

pub struct ShellWindow {
    name: &'static str,
    pages: Vec<Page>,
    selected: usize,
    nav_items: Vec<MouseStateHandle>,
    /// Close, minimise, fullscreen.
    buttons: [MouseStateHandle; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShellAction {
    Show(usize),
}

impl ShellWindow {
    pub fn new<F>(name: &'static str, build_pages: F, ctx: &mut ViewContext<Self>) -> Self
    where
        F: FnOnce(&mut ViewContext<Self>) -> Vec<Page>,
    {
        let pages = build_pages(ctx);
        let nav_items = pages.iter().map(|_| MouseStateHandle::default()).collect();
        Self {
            name,
            pages,
            selected: 0,
            nav_items,
            buttons: Default::default(),
        }
    }

    /// One window, one app: closing it quits.
    fn close_window(&mut self, _: &(), ctx: &mut ViewContext<Self>) -> bool {
        ctx.terminate_app(TerminationMode::Cancellable, None);
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

    /// A window button: its glyph centred in a 22px square that lightens on hover.
    fn button(mouse: &MouseStateHandle, glyph: Box<dyn Element>) -> Hoverable {
        let mut glyph = Some(glyph);
        Hoverable::new(mouse.clone(), move |state| {
            let background = if state.is_hovered() {
                ColorU::new(255, 255, 255, 18)
            } else {
                ColorU::transparent_black()
            };
            let glyph = glyph
                .take()
                .unwrap_or_else(|| Rect::new().finish());
            Container::new(
                ConstrainedBox::new(Align::new(glyph).finish())
                    .with_width(BUTTON_SIZE)
                    .with_height(BUTTON_SIZE)
                    .finish(),
            )
            .with_background_color(background)
            .finish()
        })
    }

    fn render_window_buttons(&self) -> Box<dyn Element> {
        let close = Self::button(
            &self.buttons[0],
            ConstrainedBox::new(Icon::new("bundled/svg/linux/decorations/close.svg", RED).finish())
                .with_width(9.)
                .with_height(9.)
                .finish(),
        )
        .on_click(|evt, _, _| evt.dispatch_action("shell:close_window", ()))
        .finish();
        let minimize = Self::button(
            &self.buttons[1],
            ConstrainedBox::new(Rect::new().with_background_color(AMBER).finish())
                .with_width(9.)
                .with_height(1.7)
                .finish(),
        )
        .on_click(|evt, _, _| evt.dispatch_action("shell:minimize_window", ()))
        .finish();
        let fullscreen = Self::button(
            &self.buttons[2],
            ConstrainedBox::new(
                Rect::new()
                    .with_border(Border::all(1.6).with_border_color(GREEN))
                    .finish(),
            )
            .with_width(10.)
            .with_height(10.)
            .finish(),
        )
        .on_click(|evt, _, _| evt.dispatch_action("shell:toggle_fullscreen", ()))
        .finish();
        ConstrainedBox::new(
            Align::new(
                Container::new(
                    Flex::row()
                        .with_children([close, minimize, fullscreen])
                        .finish(),
                )
                .with_margin_left(BUTTONS_INSET)
                .finish(),
            )
            .finish(),
        )
        .with_max_height(TITLE_BAR_HEIGHT + TITLE_BAR_BORDER)
        .with_width(BUTTONS_WIDTH)
        .finish()
    }

    fn render_title_bar(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let contents = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Container::new(
                    Text::new_inline(self.name, appearance.ui_font_family(), appearance.ui_font_size())
                        .with_color(theme.sub_text_color(theme.background()).into_solid())
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .finish(),
                )
                .with_margin_left(BUTTONS_WIDTH + 8.)
                .finish(),
            )
            .with_child(Expanded::new(1., Flex::row().finish()).finish())
            .with_child(
                Container::new(avatar::render(appearance))
                    .with_uniform_padding(2.)
                    .with_margin_right(8.)
                    .finish(),
            )
            .finish();
        let bar = Container::new(
            ConstrainedBox::new(Align::new(contents).finish())
                .with_height(TITLE_BAR_HEIGHT)
                .finish(),
        )
        .with_border(Border::bottom(TITLE_BAR_BORDER).with_border_fill(TITLE_BAR_BORDER_COLOR))
        .with_background_color(ColorU::black())
        .finish();
        let mut stack = Stack::new();
        stack.add_child(bar);
        stack.add_positioned_child(
            self.render_window_buttons(),
            OffsetPositioning::offset_from_parent(
                Vector2F::zero(),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::TopLeft,
            ),
        );
        ConstrainedBox::new(stack.finish())
            .with_height(TITLE_BAR_HEIGHT + TITLE_BAR_BORDER)
            .finish()
    }

    fn render_left_bar(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut items = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (index, (page, mouse)) in self.pages.iter().zip(&self.nav_items).enumerate() {
            let variant = if index == self.selected {
                ButtonVariant::Accent
            } else {
                ButtonVariant::Text
            };
            items.add_child(
                appearance
                    .ui_builder()
                    .button(variant, mouse.clone())
                    .with_text_label(page.label.to_owned())
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
                        ctx.dispatch_typed_action(ShellAction::Show(index));
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

impl Entity for ShellWindow {
    type Event = ();
}

impl View for ShellWindow {
    fn ui_name() -> &'static str {
        "AppShellWindow"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut body = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_left_bar(appearance));
        if let Some(page) = self.pages.get(self.selected) {
            body.add_child(Expanded::new(1., (page.render)()).finish());
        }
        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(self.render_title_bar(appearance))
                .with_child(Expanded::new(1., body.finish()).finish())
                .finish(),
        )
        .with_background(appearance.theme().background())
        .finish()
    }
}

impl TypedActionView for ShellWindow {
    type Action = ShellAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ShellAction::Show(index) => {
                self.selected = (*index).min(self.pages.len().saturating_sub(1));
                ctx.notify();
            }
        }
    }
}
