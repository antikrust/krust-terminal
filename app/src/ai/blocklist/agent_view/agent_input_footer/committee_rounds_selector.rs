//! The committee's round plan, as a menu next to the model picker.
//!
//! The plan was previously settable three ways — a settings page two clicks
//! away, a `rounds:` prefix typed into the prompt, and a `round <kind>`
//! follow-up after a verdict — and all three ask the user to remember
//! something. This is the fourth and the one that doesn't: the control sits
//! where the decision is made, next to the model it applies to, and shows the
//! current plan without being asked.
//!
//! It writes the same setting the Ollama page writes (`panel_plan` inside
//! `ollama_committee_config`), so the two surfaces can never disagree.

use std::sync::Arc;

use pathfinder_geometry::vector::vec2f;
use settings::Setting;
use warp_core::ui::theme::Fill;
use warp_errors::report_if_error;
use warpui::elements::{
    ChildAnchor, ChildView, ConstrainedBox, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Stack,
};
use warpui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::AgentInputButtonTheme;
use crate::ai::agent_sdk::driver::harness::ollama_committee::{
    OllamaCommitteeConfig, PanelRoundKind,
};
use crate::appearance::Appearance;
use crate::context_chips::display_menu::{
    ChipMenuType, DisplayChipMenu, GenericMenuItem, PromptDisplayMenuEvent,
};
use crate::settings::AISettings;
use crate::terminal::input::{MenuPositioning, MenuPositioningProvider};
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{ActionButton, ButtonSize};

const ROUNDS_TOOLTIP: &str = "How many times the committee argues before the verdict";
const CHECK_ICON_SIZE: f32 = 16.;

/// One offered plan. Deliberately a short list of *whole plans* rather than
/// per-slot controls: "notes then rewrite" is the decision a user makes, and
/// two dropdowns that compose into it is the settings page's job, not the
/// input bar's.
struct RoundPlan {
    label: &'static str,
    detail: &'static str,
    kinds: &'static [PanelRoundKind],
}

const PLANS: [RoundPlan; 8] = [
    RoundPlan {
        label: "Straight to the verdict",
        detail: "everyone answers once, the judge picks",
        kinds: &[],
    },
    RoundPlan {
        label: "Judge notes",
        detail: "the judge marks each answer, everyone revises",
        kinds: &[PanelRoundKind::Notes],
    },
    RoundPlan {
        label: "Judge's rewrite",
        detail: "the judge rewrites the best answer, everyone responds",
        kinds: &[PanelRoundKind::Exemplar],
    },
    RoundPlan {
        label: "Peer critique",
        detail: "panelists tear into each other, no judge",
        kinds: &[PanelRoundKind::Peers],
    },
    RoundPlan {
        label: "Converge on the leader",
        detail: "everyone builds on the current best answer",
        kinds: &[PanelRoundKind::Converge],
    },
    RoundPlan {
        label: "Judge notes ×2",
        detail: "two rounds of marking and revising",
        kinds: &[PanelRoundKind::Notes, PanelRoundKind::Notes],
    },
    RoundPlan {
        label: "Notes → rewrite",
        detail: "marked once, then handed the judge's own version",
        kinds: &[PanelRoundKind::Notes, PanelRoundKind::Exemplar],
    },
    RoundPlan {
        label: "Peers → converge",
        detail: "tear into each other, then agree",
        kinds: &[PanelRoundKind::Peers, PanelRoundKind::Converge],
    },
];

/// The plan's own name, for the button. Falls back to spelling the plan out
/// when a hand-edited setting doesn't match anything offered here.
fn plan_label(plan: &[PanelRoundKind]) -> String {
    PLANS
        .iter()
        .find(|offered| offered.kinds == plan)
        .map(|offered| offered.label.to_string())
        .unwrap_or_else(|| {
            plan.iter()
                .map(|kind| kind.display_name())
                .collect::<Vec<_>>()
                .join(" → ")
        })
}

#[derive(Debug, Clone)]
struct RoundPlanMenuItem {
    label: String,
    detail: String,
    kinds: Vec<PanelRoundKind>,
    is_selected: bool,
}

impl GenericMenuItem for RoundPlanMenuItem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> String {
        format!("{} — {}", self.label, self.detail)
    }

    fn icon(&self, _app: &AppContext) -> Option<Icon> {
        None
    }

    fn action_data(&self) -> String {
        self.label.clone()
    }

    fn right_side_element(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if !self.is_selected {
            return None;
        }
        let theme = Appearance::as_ref(app).theme();
        let color = theme.main_text_color(theme.surface_2()).into_solid();
        Some(
            ConstrainedBox::new(Icon::Check.to_warpui_icon(Fill::Solid(color)).finish())
                .with_width(CHECK_ICON_SIZE)
                .with_height(CHECK_ICON_SIZE)
                .finish(),
        )
    }
}

pub struct CommitteeRoundsSelector {
    button: ViewHandle<ActionButton>,
    dropdown: ViewHandle<DisplayChipMenu>,
    is_menu_open: bool,
    menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
}

pub enum CommitteeRoundsSelectorEvent {
    MenuVisibilityChanged { open: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitteeRoundsSelectorAction {
    ToggleMenu,
}

impl CommitteeRoundsSelector {
    pub fn new(
        menu_positioning_provider: Arc<dyn MenuPositioningProvider>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("", AgentInputButtonTheme)
                .with_icon(Icon::Repeat)
                .with_tooltip(ROUNDS_TOOLTIP)
                .with_size(ButtonSize::AgentInputButton)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(CommitteeRoundsSelectorAction::ToggleMenu);
                })
        });

        let dropdown = ctx.add_typed_action_view(move |ctx| {
            DisplayChipMenu::new(
                Vec::<RoundPlanMenuItem>::new(),
                None,
                ChipMenuType::CommitteeRounds,
                ctx,
            )
        });

        ctx.subscribe_to_view(&dropdown, |me, _, event, ctx| {
            if let PromptDisplayMenuEvent::MenuAction(action) = event {
                if let Some(item) = action
                    .action_item
                    .as_any()
                    .downcast_ref::<RoundPlanMenuItem>()
                {
                    me.select_plan(item.kinds.clone(), ctx);
                }
                me.set_menu_visibility(false, ctx);
            }
        });

        let mut selector = Self {
            button,
            dropdown,
            is_menu_open: false,
            menu_positioning_provider,
        };
        selector.refresh(ctx);
        selector
    }

    /// The stored plan, straight from the setting the Ollama page writes.
    fn current_plan(&self, ctx: &ViewContext<Self>) -> Vec<PanelRoundKind> {
        OllamaCommitteeConfig::from_settings_json(
            AISettings::as_ref(ctx)
                .ollama_committee_config
                .value()
                .as_deref(),
        )
        .panel_plan
    }

    fn select_plan(&mut self, kinds: Vec<PanelRoundKind>, ctx: &mut ViewContext<Self>) {
        let mut config = OllamaCommitteeConfig::from_settings_json(
            AISettings::as_ref(ctx)
                .ollama_committee_config
                .value()
                .as_deref(),
        );
        config.panel_plan = kinds;
        report_if_error!(AISettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .ollama_committee_config
                .set_value(Some(config.to_settings_json()), ctx)
        }));
        self.refresh(ctx);
    }

    /// Re-reads the setting into both the button's label and the menu's
    /// checkmark — the two must never disagree, so they refresh together.
    pub fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let plan = self.current_plan(ctx);
        let label = plan_label(&plan);
        self.button.update(ctx, |button, ctx| {
            button.set_label(format!("Rounds: {label}"), ctx);
        });
        let items: Vec<RoundPlanMenuItem> = PLANS
            .iter()
            .map(|offered| RoundPlanMenuItem {
                label: offered.label.to_string(),
                detail: offered.detail.to_string(),
                kinds: offered.kinds.to_vec(),
                is_selected: offered.kinds == plan.as_slice(),
            })
            .collect();
        self.dropdown.update(ctx, |menu, ctx| {
            menu.update_menu_items(items, ctx);
        });
        ctx.notify();
    }

    pub fn is_menu_open(&self) -> bool {
        self.is_menu_open
    }

    fn set_menu_visibility(&mut self, open: bool, ctx: &mut ViewContext<Self>) {
        if self.is_menu_open == open {
            return;
        }
        self.is_menu_open = open;
        ctx.emit(CommitteeRoundsSelectorEvent::MenuVisibilityChanged { open });
        ctx.notify();
    }

    fn menu_positioning(&self, app: &AppContext) -> OffsetPositioning {
        match self.menu_positioning_provider.menu_position(app) {
            MenuPositioning::BelowInputBox => OffsetPositioning::offset_from_parent(
                vec2f(0., 4.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::BottomLeft,
                ChildAnchor::TopLeft,
            ),
            MenuPositioning::AboveInputBox => OffsetPositioning::offset_from_parent(
                vec2f(0., -4.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            ),
        }
    }
}

impl TypedActionView for CommitteeRoundsSelector {
    type Action = CommitteeRoundsSelectorAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CommitteeRoundsSelectorAction::ToggleMenu => {
                let open = !self.is_menu_open;
                if open {
                    // The Ollama settings page writes the same setting, so
                    // the menu is rebuilt on every open rather than trusted.
                    self.refresh(ctx);
                }
                self.set_menu_visibility(open, ctx);
            }
        }
    }
}

impl View for CommitteeRoundsSelector {
    fn ui_name() -> &'static str {
        "CommitteeRoundsSelector"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut stack = Stack::new();
        stack.add_child(ChildView::new(&self.button).finish());
        if self.is_menu_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.dropdown).finish(),
                self.menu_positioning(app),
            );
        }
        stack.finish()
    }
}

impl Entity for CommitteeRoundsSelector {
    type Event = CommitteeRoundsSelectorEvent;
}
