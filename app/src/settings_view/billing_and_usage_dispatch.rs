//! Dispatch wrapper that routes between the legacy and v2 billing & usage
//! pages.
//!
//! This is no longer a standalone settings page. It is embedded at the bottom
//! of the Account page (see `main_page::BillingAndUsageWidget`), so it renders
//! its active child directly and leaves scrolling/padding to the host page.

use warp_core::features::FeatureFlag;
use warpui::elements::{ChildView, Element};
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext, ViewHandle};

use super::billing_and_usage_page::{BillingAndUsagePageEvent, BillingAndUsagePageView};
use super::billing_and_usage_page_v2::BillingAndUsagePageV2View;
use crate::auth::AuthManager;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::workspaces::workspace::Workspace;

pub struct BillingAndUsageDispatchView {
    v1: ViewHandle<BillingAndUsagePageView>,
    v2: ViewHandle<BillingAndUsagePageV2View>,
}

impl BillingAndUsageDispatchView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let v1 = ctx.add_typed_action_view(BillingAndUsagePageView::new);
        let v2 = ctx.add_typed_action_view(BillingAndUsagePageV2View::new);

        // Both children stay alive; only forward events from the active one
        // to avoid duplicate toasts.
        ctx.subscribe_to_view(&v1, |this, _, event, ctx| {
            if !this.use_v2(ctx) {
                ctx.emit(event.clone());
            }
        });
        ctx.subscribe_to_view(&v2, |this, _, event, ctx| {
            if this.use_v2(ctx) {
                ctx.emit(event.clone());
            }
        });

        ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });

        Self { v1, v2 }
    }

    fn use_v2(&self, ctx: &AppContext) -> bool {
        if !FeatureFlag::BillingAndUsagePageV2.is_enabled() {
            return false;
        }
        Self::workspace_uses_v2(UserWorkspaces::as_ref(ctx).current_workspace())
    }

    fn workspace_uses_v2(workspace: Option<&Workspace>) -> bool {
        workspace.is_none_or(|workspace| {
            let bm = &workspace.billing_metadata;
            bm.is_on_build_plan()
                || bm.is_on_build_max_plan()
                || bm.is_on_build_business_plan()
                || bm.is_enterprise_plan()
                || bm.is_free_plan()
        })
    }

    pub fn get_modal_content(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if self.use_v2(app) {
            self.v2.read(app, |view, _| view.get_modal_content())
        } else {
            self.v1.read(app, |view, _| view.get_modal_content())
        }
    }

    /// Forwarded from the host page's `on_page_selected` so the active child
    /// refreshes its usage data when the Account page is opened.
    pub(super) fn on_page_selected(
        &mut self,
        allow_steal_focus: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.use_v2(ctx) {
            self.v2.update(ctx, |view, ctx| {
                view.on_page_selected(allow_steal_focus, ctx)
            });
        } else {
            self.v1.update(ctx, |view, ctx| {
                view.on_page_selected(allow_steal_focus, ctx)
            });
        }
    }
}

#[cfg(test)]
#[path = "billing_and_usage_dispatch_tests.rs"]
mod tests;

impl Entity for BillingAndUsageDispatchView {
    type Event = BillingAndUsagePageEvent;
}

impl View for BillingAndUsageDispatchView {
    fn ui_name() -> &'static str {
        "Billing and usage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        if self.use_v2(app) {
            ChildView::new(&self.v2).finish()
        } else {
            ChildView::new(&self.v1).finish()
        }
    }
}
