use std::sync::Arc;

use parking_lot::FairMutex;
use borkui::elements::{
    ChildView, Container, CrossAxisAlignment, Expanded, Flex, MainAxisSize, ParentElement,
};
use borkui::prelude::Empty;
use borkui::{AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle};

use super::{AgentFooterButtonTheme, USE_AGENT_KEYSTROKE};
use crate::terminal::view::{TerminalModel, PADDING_LEFT};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, KeystrokeSource, TooltipAlignment,
};

/// Footer view rendered for detected subshell commands, offering both
/// "Borkify" and "Use agent" buttons in a horizontal row.
pub(super) struct BorkifyFooterView {
    terminal_model: Arc<FairMutex<TerminalModel>>,
    borkify_button: ViewHandle<ActionButton>,
    use_agent_button: ViewHandle<ActionButton>,
    dismiss_button: ViewHandle<ActionButton>,
    /// Whether the footer is currently offering subshell borkification.
    is_active: bool,
}

impl BorkifyFooterView {
    pub fn new(terminal_model: Arc<FairMutex<TerminalModel>>, ctx: &mut ViewContext<Self>) -> Self {
        let button_size = ButtonSize::XSmall;

        let borkify_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Borkify subshell", AgentFooterButtonTheme::new(None))
                .with_icon(Icon::Bork)
                .with_size(button_size)
                .with_tooltip("Enable Bork shell integration in this session")
                .with_tooltip_alignment(TooltipAlignment::Left)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(BorkifyFooterViewAction::Borkify);
                })
        });

        let use_agent_button = ctx.add_typed_action_view(|ctx| {
            ActionButton::new("Use agent", AgentFooterButtonTheme::new(None))
                .with_icon(Icon::Oz)
                .with_keybinding(KeystrokeSource::Fixed(USE_AGENT_KEYSTROKE.clone()), ctx)
                .with_size(button_size)
                .with_tooltip("Ask the Bork agent to assist")
                .with_tooltip_alignment(TooltipAlignment::Left)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(BorkifyFooterViewAction::UseAgent);
                })
        });

        let dismiss_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Dismiss", AgentFooterButtonTheme::new(None))
                .with_size(button_size)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(BorkifyFooterViewAction::Dismiss);
                })
        });

        Self {
            terminal_model,
            borkify_button,
            use_agent_button,
            dismiss_button,
            is_active: false,
        }
    }

    /// Activates the footer so it offers subshell borkification.
    pub fn show(&mut self, ctx: &mut ViewContext<Self>) {
        self.borkify_button.update(ctx, |button, ctx| {
            button.set_keybinding(
                Some(KeystrokeSource::Binding("terminal:borkify_subshell")),
                ctx,
            );
        });
        self.is_active = true;
        ctx.notify();
    }

    /// Returns whether the footer is currently offering subshell borkification.
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Deactivates the footer.
    pub fn clear(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_active = false;
        self.borkify_button.update(ctx, |button, ctx| {
            button.set_keybinding(None, ctx);
        });
        ctx.notify();
    }
}

#[derive(Debug, Clone)]
pub enum BorkifyFooterViewAction {
    Borkify,
    UseAgent,
    Dismiss,
}

pub enum BorkifyFooterViewEvent {
    Borkify,
    UseAgent,
    Dismiss,
}

impl Entity for BorkifyFooterView {
    type Event = BorkifyFooterViewEvent;
}

impl View for BorkifyFooterView {
    fn ui_name() -> &'static str {
        "BorkifyFooterView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        let terminal_model = self.terminal_model.lock();

        let button_row = Flex::row()
            .with_spacing(4.)
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(ChildView::new(&self.borkify_button).finish())
            .with_child(ChildView::new(&self.use_agent_button).finish())
            .with_child(Expanded::new(1., Empty::new().finish()).finish())
            .with_child(ChildView::new(&self.dismiss_button).finish());

        let mut container = Container::new(button_row.finish())
            .with_horizontal_padding(*PADDING_LEFT)
            .with_vertical_padding(4.);

        if terminal_model.is_alt_screen_active() {
            if let Some(bg_color) = terminal_model.alt_screen().inferred_bg_color() {
                container = container.with_background(bg_color);
            }
        }

        container.finish()
    }
}

impl TypedActionView for BorkifyFooterView {
    type Action = BorkifyFooterViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            BorkifyFooterViewAction::Borkify => {
                if self.is_active {
                    self.clear(ctx);
                    ctx.emit(BorkifyFooterViewEvent::Borkify);
                }
            }
            BorkifyFooterViewAction::UseAgent => {
                self.clear(ctx);
                ctx.emit(BorkifyFooterViewEvent::UseAgent);
            }
            BorkifyFooterViewAction::Dismiss => {
                self.clear(ctx);
                ctx.emit(BorkifyFooterViewEvent::Dismiss);
            }
        }
    }
}
