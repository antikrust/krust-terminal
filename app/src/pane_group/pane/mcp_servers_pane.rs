//! Fork: a dedicated MCP Servers pane, opened from the Library.
//!
//! Upstream sent the Library's "MCP Servers" row to the settings window,
//! yanking the user out of their terminal context. This pane hosts the
//! existing [`MCPServersSettingsPageView`] (list, add/edit, and its modals)
//! in a split pane instead, mirroring how the Rules row opens
//! [`crate::pane_group::AIFactPane`]. The settings page under
//! Agents → MCP servers still exists and shares the same underlying data.
//!
//! Modeled on [`super::network_log_pane::NetworkLogPane`]: at most one pane
//! per window (tracked by [`MCPServersPaneManager`]), and not persisted
//! across restarts (`LeafContents::MCPServers` is non-persisted; reopening
//! from the Library is one click).

use std::collections::HashMap;

use warpui::elements::{
    ChildAnchor, ChildView, Container, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Stack,
};
use warpui::{
    AppContext, Element, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use super::view::PaneView;
use super::{
    view, DetachType, PaneConfiguration, PaneContent, PaneEvent, PaneGroup, PaneId, ShareableLink,
    ShareableLinkError,
};
use crate::app_state::LeafContents;
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::BackingView;
use crate::settings_view::mcp_servers_page::MCPServersSettingsPageView;
use crate::workspace::PaneViewLocator;

const HEADER_TEXT: &str = "MCP Servers";
const PANE_PADDING: f32 = 16.;

#[derive(Debug, Clone)]
pub enum MCPServersPaneViewEvent {
    Pane(PaneEvent),
}

/// The pane's backing view: hosts the MCP servers settings page standalone,
/// providing the scrolling and modal overlay the settings container would
/// normally supply.
pub struct MCPServersPaneView {
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
    page_view: ViewHandle<MCPServersSettingsPageView>,
}

impl MCPServersPaneView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_ctx| PaneConfiguration::new(HEADER_TEXT));

        let page_view = ctx.add_typed_action_view(MCPServersSettingsPageView::new);
        // The page's events (ShowModal / HideModal) only signal that the
        // modal overlay rendered below needs a refresh.
        ctx.subscribe_to_view(&page_view, |_me, _, _event, ctx| ctx.notify());

        Self {
            pane_configuration,
            focus_handle: None,
            page_view,
        }
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        let page_view = self.page_view.clone();
        page_view.update(ctx, |page, ctx| page.focus(ctx));
    }
}

impl Entity for MCPServersPaneView {
    type Event = MCPServersPaneViewEvent;
}

impl View for MCPServersPaneView {
    fn ui_name() -> &'static str {
        "MCPServersPaneView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            self.focus(ctx);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        // The page was built with its own dual scrollables
        // (`PageType::new_monolith(..., true)`), so it must get the pane's
        // finite bounds directly. Wrapping it in another scrollable hands
        // its internal flex an infinite height constraint, which panics
        // the layout ("flex contains flexible children but has an infinite
        // constraint along the flex axis").
        let base = Container::new(ChildView::new(&self.page_view).finish())
            .with_uniform_padding(PANE_PADDING)
            .finish();

        // The settings container normally overlays the page's modals
        // (installation confirmation, deletion prompts); do the same here.
        let mut stack = Stack::new().with_child(base);
        if let Some(modal_content) = self
            .page_view
            .read(app, |page, _| page.get_modal_content(app))
        {
            stack.add_positioned_overlay_child(
                modal_content,
                OffsetPositioning::offset_from_parent(
                    pathfinder_geometry::vector::vec2f(0., 0.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::Center,
                    ChildAnchor::Center,
                ),
            );
        }
        stack.finish()
    }
}

impl TypedActionView for MCPServersPaneView {
    type Action = ();

    fn handle_action(&mut self, _action: &Self::Action, _ctx: &mut ViewContext<Self>) {}
}

impl BackingView for MCPServersPaneView {
    type PaneHeaderOverflowMenuAction = ();
    type CustomAction = ();
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        _action: &Self::PaneHeaderOverflowMenuAction,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(MCPServersPaneViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        _app: &AppContext,
    ) -> view::HeaderContent {
        view::HeaderContent::simple(HEADER_TEXT)
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}

pub struct MCPServersPane {
    view: ViewHandle<PaneView<MCPServersPaneView>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
}

impl MCPServersPane {
    pub fn from_view(
        mcp_servers_pane_view: ViewHandle<MCPServersPaneView>,
        ctx: &mut AppContext,
    ) -> Self {
        let pane_configuration = mcp_servers_pane_view.as_ref(ctx).pane_configuration();

        let view = ctx.add_typed_action_view(mcp_servers_pane_view.window_id(ctx), |ctx| {
            let pane_id = PaneId::from_mcp_servers_pane_ctx(ctx);
            PaneView::new(
                pane_id,
                mcp_servers_pane_view,
                (),
                pane_configuration.clone(),
                ctx,
            )
        });

        Self {
            view,
            pane_configuration,
        }
    }

    pub fn new<V: View>(ctx: &mut ViewContext<V>) -> Self {
        let view = ctx.add_typed_action_view(MCPServersPaneView::new);
        Self::from_view(view, ctx)
    }

    pub fn mcp_servers_pane_view(&self, ctx: &AppContext) -> ViewHandle<MCPServersPaneView> {
        self.view.as_ref(ctx).child(ctx)
    }
}

impl PaneContent for MCPServersPane {
    fn id(&self) -> PaneId {
        PaneId::from_mcp_servers_pane_view(&self.view)
    }

    fn snapshot(&self, _app: &AppContext) -> LeafContents {
        LeafContents::MCPServers
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        focus_handle: PaneFocusHandle,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        self.view
            .update(ctx, |view, ctx| view.set_focus_handle(focus_handle, ctx));

        let mcp_servers_pane_view = self.mcp_servers_pane_view(ctx);
        let pane_id = self.id();
        let pane_group_id = ctx.view_id();
        let window_id = ctx.window_id();

        ctx.subscribe_to_view(&mcp_servers_pane_view, move |pane_group, _, event, ctx| {
            let MCPServersPaneViewEvent::Pane(pane_event) = event;
            pane_group.handle_pane_event(pane_id, pane_event, ctx)
        });
        ctx.subscribe_to_view(&self.view, move |group, _, event, ctx| {
            group.handle_pane_view_event(pane_id, event, ctx);
        });

        MCPServersPaneManager::handle(ctx).update(ctx, |manager, _ctx| {
            manager.register_pane(
                window_id,
                PaneViewLocator {
                    pane_group_id,
                    pane_id,
                },
            );
        });
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        _detach_type: DetachType,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        // Always unsubscribe from views.
        let mcp_servers_pane_view = self.mcp_servers_pane_view(ctx);
        ctx.unsubscribe_to_view(&mcp_servers_pane_view);
        ctx.unsubscribe_to_view(&self.view);

        // Always deregister from the manager.
        let window_id = ctx.window_id();
        MCPServersPaneManager::handle(ctx).update(ctx, |manager, _| {
            manager.deregister_pane(&window_id);
        });
    }

    fn has_application_focus(&self, ctx: &mut ViewContext<PaneGroup>) -> bool {
        self.view.is_self_or_child_focused(ctx)
    }

    fn focus(&self, ctx: &mut ViewContext<PaneGroup>) {
        self.mcp_servers_pane_view(ctx)
            .update(ctx, |view, ctx| view.focus(ctx));
    }

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<ShareableLink, ShareableLinkError> {
        Ok(ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, ctx: &AppContext) -> bool {
        self.view.as_ref(ctx).is_being_dragged()
    }
}

/// Singleton that maintains a map of `WindowId -> PaneViewLocator` for any
/// open MCP servers panes, so each window shows at most one and reopening
/// focuses the existing pane.
#[derive(Default)]
pub struct MCPServersPaneManager {
    panes: HashMap<WindowId, PaneViewLocator>,
}

impl MCPServersPaneManager {
    pub fn find_pane(&self, window_id: WindowId) -> Option<PaneViewLocator> {
        self.panes.get(&window_id).copied()
    }

    pub fn register_pane(&mut self, window_id: WindowId, locator: PaneViewLocator) {
        self.panes.insert(window_id, locator);
    }

    pub fn deregister_pane(&mut self, window_id: &WindowId) {
        self.panes.remove(window_id);
    }
}

impl Entity for MCPServersPaneManager {
    type Event = ();
}

impl SingletonEntity for MCPServersPaneManager {}
