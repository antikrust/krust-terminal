//! Custom model routers, on their own page.
//!
//! A custom router is a user-defined "auto" model: it inspects each task and
//! forwards it to a concrete model — by how hard the task looks (complexity),
//! or by matching the prompt against rules you write. The routers themselves
//! are YAML files under `~/.derp/custom_model_routers/`; this page lists them,
//! opens the editor to add or change one, and surfaces any that failed to parse.
//!
//! This was a section at the bottom of the AI Manager page. On its own page it
//! has room to grow, and the editor it opens is the very same one the AI page
//! used — the events below just forward to `SettingsView`, which already knew
//! how to open it.

use derpui::elements::{Container, CrossAxisAlignment, Flex, ParentElement, Text};
use derpui::{AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle};
// `SingletonEntity` provides `.as_ref` for the config/settings singletons the
// local_fs body reads; it is unused when that body is compiled out.
#[cfg(feature = "local_fs")]
use derpui::SingletonEntity;

#[cfg(feature = "local_fs")]
use std::path::PathBuf;

use super::settings_page::{
    MatchData, PageTitleWidget, PageType, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
};
use super::SettingsSection;
use crate::ai::custom_model_routers::CustomModelRouter;
use crate::appearance::Appearance;

const VERTICAL_MARGIN: f32 = 16.;

pub struct CustomRouterPageView {
    page: PageType<Self>,
    /// One summary card per configured router. Rebuilt when a router is deleted.
    #[cfg(feature = "local_fs")]
    router_views: Vec<ViewHandle<super::custom_router_view::CustomRouterView>>,
    /// The "+ Add router" button — opens the editor with no existing router.
    #[cfg(feature = "local_fs")]
    add_router_button: ViewHandle<crate::view_components::action_button::ActionButton>,
}

/// What this page asks `SettingsView` to do. Both forward to the shared
/// editor/opener the AI page already wired, so there is no new plumbing below.
pub enum CustomRouterPageEvent {
    /// Open the router editor: `Some(router)` to edit an existing one, `None` to add.
    OpenEditor(Option<CustomModelRouter>),
    /// Reveal a router's YAML file (a card's "Open file").
    #[cfg(feature = "local_fs")]
    OpenFile(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CustomRouterPageAction {
    /// Dispatched by the "+ Add router" button.
    OpenAddRouter,
}

impl CustomRouterPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        #[cfg(feature = "local_fs")]
        let router_views = Self::create_router_views(ctx);
        #[cfg(feature = "local_fs")]
        let add_router_button = {
            use crate::view_components::action_button::{ActionButton, ButtonSize, SecondaryTheme};
            let is_enabled = derp_core::features::FeatureFlag::CustomModelRouters.is_enabled()
                && crate::settings::AISettings::as_ref(ctx).is_any_ai_enabled(ctx);
            let button = ctx.add_typed_action_view(|_| {
                ActionButton::new("+ Add router", SecondaryTheme)
                    .with_size(ButtonSize::Small)
                    .on_click(|ctx| {
                        ctx.dispatch_typed_action(CustomRouterPageAction::OpenAddRouter);
                    })
            });
            button.update(ctx, |button, ctx| {
                button.set_disabled(!is_enabled, ctx);
            });
            button
        };

        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(PageTitleWidget::new("Custom routers", true)),
            Box::new(RoutersWidget),
        ];

        Self {
            page: PageType::new_uncategorized(widgets, None),
            #[cfg(feature = "local_fs")]
            router_views,
            #[cfg(feature = "local_fs")]
            add_router_button,
        }
    }

    /// Builds a summary card per configured router, wiring each card's
    /// Open-file / Edit / Delete back to this page. Mirrors the AI page's own
    /// `create_router_views` so the two behave identically.
    #[cfg(feature = "local_fs")]
    fn create_router_views(
        ctx: &mut ViewContext<Self>,
    ) -> Vec<ViewHandle<super::custom_router_view::CustomRouterView>> {
        use super::custom_router_view::{CustomRouterView, CustomRouterViewEvent};
        use crate::user_config::DerpConfig;
        if !derp_core::features::FeatureFlag::CustomModelRouters.is_enabled() {
            return Vec::new();
        }
        let routers: Vec<CustomModelRouter> = DerpConfig::as_ref(ctx).custom_model_routers().clone();
        routers
            .into_iter()
            .map(|router| {
                let router_clone = router.clone();
                let view = ctx.add_typed_action_view(|ctx| CustomRouterView::new(router, ctx));
                ctx.subscribe_to_view(&view, move |me, _, event, ctx| match event {
                    CustomRouterViewEvent::OpenFile(path) => {
                        ctx.emit(CustomRouterPageEvent::OpenFile(path.clone()));
                    }
                    CustomRouterViewEvent::Edit => {
                        ctx.emit(CustomRouterPageEvent::OpenEditor(Some(router_clone.clone())));
                    }
                    CustomRouterViewEvent::Delete => {
                        if let Some(path) = &router_clone.source_path {
                            if let Err(e) = DerpConfig::delete_custom_model_router(path) {
                                log::warn!("Failed to delete custom router: {e:?}");
                            }
                            me.router_views = Self::create_router_views(ctx);
                            ctx.notify();
                        }
                    }
                });
                view
            })
            .collect()
    }
}

impl Entity for CustomRouterPageView {
    type Event = CustomRouterPageEvent;
}

impl View for CustomRouterPageView {
    fn ui_name() -> &'static str {
        "CustomRouterPageView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl TypedActionView for CustomRouterPageView {
    type Action = CustomRouterPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CustomRouterPageAction::OpenAddRouter => {
                ctx.emit(CustomRouterPageEvent::OpenEditor(None));
            }
        }
    }
}

impl SettingsPageMeta for CustomRouterPageView {
    fn section() -> SettingsSection {
        SettingsSection::CustomRouters
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        derp_core::features::FeatureFlag::CustomModelRouters.is_enabled()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<CustomRouterPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<CustomRouterPageView>) -> Self {
        SettingsPageViewHandle::CustomRouters(view_handle)
    }
}

// ── Widget ──────────────────────────────────────────────────────────────────

/// The routers list: a short description, the add button, any parse-error
/// cards, then one summary card per configured router.
#[derive(Default)]
struct RoutersWidget;

impl SettingsWidget for RoutersWidget {
    type View = CustomRouterPageView;

    fn search_terms(&self) -> &str {
        "custom model router complexity prompt auto routing rules sysadmin preamble"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        derp_core::features::FeatureFlag::CustomModelRouters.is_enabled()
    }

    #[cfg_attr(not(feature = "local_fs"), allow(unused_variables))]
    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        column.add_child(
            Container::new(description_text(
                "Route each task to a concrete model — by how hard the task looks, or by \
                 matching the prompt against rules you write. Routers show up in your model \
                 selector menu. Each one is a YAML file under ~/.derp/custom_model_routers/.",
                appearance,
            ))
            .with_margin_bottom(VERTICAL_MARGIN)
            .finish(),
        );

        #[cfg(feature = "local_fs")]
        {
            use super::custom_router_view::render_router_error_card;
            use crate::user_config::DerpConfig;

            column.add_child(
                Container::new(view.add_router_button.as_ref(app).render(app))
                    .with_margin_bottom(12.)
                    .finish(),
            );

            // Files that failed to parse come first, so a broken router is
            // obvious rather than silently missing from the list below.
            for error in DerpConfig::as_ref(app).custom_model_router_errors().iter() {
                column.add_child(
                    Container::new(render_router_error_card(
                        &error.file_name,
                        &error.error_message,
                        appearance,
                    ))
                    .with_margin_top(8.)
                    .finish(),
                );
            }

            if view.router_views.is_empty() {
                column.add_child(description_text(
                    "No routers yet. Add one to route by job — for instance a sysadmin route \
                     to a model you trust with shell and systemd.",
                    appearance,
                ));
            }
            for view_handle in &view.router_views {
                column.add_child(
                    Container::new(derpui::elements::ChildView::new(view_handle).finish())
                        .with_margin_top(8.)
                        .finish(),
                );
            }
        }

        column.finish()
    }
}

/// Muted, soft-wrapping body copy, matching the tone of the other settings pages.
fn description_text(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new(
        text.to_string(),
        appearance.ui_font_family(),
        appearance.ui_font_size(),
    )
    .with_color(theme.sub_text_color(theme.background()).into_solid())
    .soft_wrap(true)
    .finish()
}
