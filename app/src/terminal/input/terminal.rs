use krust_core::settings::Setting;
use krustui::elements::{
    Border, Container, CrossAxisAlignment, DropTarget, Element, Empty, Expanded, Flex, Hoverable,
    ParentElement, SavePosition, Stack,
};
use krustui::presenter::ChildView;
use krustui::{AppContext, SingletonEntity};

use super::common::{
    add_command_xray_overlay, add_input_suggestions_overlays, add_voltron_overlay,
    add_workflow_info_overlay, wrap_input_with_terminal_padding_and_focus_handler,
};
use super::{Input, InputAction, InputDropTargetData};

use crate::appearance::Appearance;
use crate::context_chips::spacing;
use crate::features::FeatureFlag;
use crate::settings::{AppEditorSettings, InputModeSettings};
use crate::terminal::block_list_settings::BlockListSettings;
use crate::terminal::block_list_viewport::InputMode;
use crate::terminal::view::TerminalAction;

/// Vertical space above the input row, balancing the editor's own bottom padding so the
/// text sits centred on its line rather than hard against the divider above it.
const COMPACT_ROW_TOP_PADDING: f32 = 6.;

/// Extra drop applied to the prompt and editor only. They read a touch high against the
/// chips at the same top alignment, and the chips are already where they should be.
const COMMAND_LINE_TOP_NUDGE: f32 = 5.;

/// Below this pane width the context chips move to their own line beneath the command
/// rather than crowd it. Roughly where the chips plus a usable command line stop fitting.
const MIN_PANE_WIDTH_FOR_CHIPS: f32 = 700.;

/// Space between the command line and the chips when they have dropped below it.
const CHIPS_BELOW_COMMAND_GAP: f32 = 4.;

impl Input {
    /// Renders the terminal mode input when `FeatureFlag::AgentView` is enabled and there is no
    /// active agent view.
    pub(super) fn render_terminal_input(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let menu_positioning = self.menu_positioning(app);

        let model = self.model.lock();

        // We should likely rework this stack to not need to use `with_constrain_absolute_children`,
        // by reworking the positioning of the children to not depend on this.
        let mut stack = Stack::new().with_constrain_absolute_children();

        let vim_state = self.editor.as_ref(app).vim_state(app);
        let app_editor_settings = AppEditorSettings::as_ref(app);
        let show_vim_status = vim_state.is_some() && *app_editor_settings.vim_status_bar.value();
        let input_mode = *InputModeSettings::as_ref(app).input_mode.value();

        let mut column = Flex::column();

        if matches!(input_mode, InputMode::PinnedToBottom | InputMode::Waterfall) {
            if let Some(banner) = self.render_input_banner(appearance, app, input_mode, false) {
                column.add_child(
                    Container::new(banner)
                        .with_margin_top(spacing::UDI_CHIP_MARGIN)
                        .finish(),
                );
            }
        }

        // The compact single-line layout is the design of this terminal, not a preference:
        // the shell prompt sits on the left, the editor fills the row and grows downwards as
        // the command wraps, and the folder chip is pushed to the far right. No hint bar.
        let shell_prompt = self
            .prompt_render_helper
            .render_terminal_prompt(&model, appearance, app);

        // The chips take their natural width, so beside a command line in a narrow pane they
        // squeeze it to nothing and spill past the edge. The prompt and what you are typing
        // must never lose that fight — but the chips carry real information, so rather than
        // dropping them they drop *down*, onto their own line beneath the command, where the
        // full pane width is theirs.
        let chips_fit_beside_the_command =
            self.size_info(app).pane_width_px().as_f32() >= MIN_PANE_WIDTH_FOR_CHIPS;
        let folder_chip = self
            .prompt_render_helper
            .render_terminal_folder_chip(&model, appearance, app);

        let input_box = self.render_input_box(show_vim_status, appearance, app);

        // Everything is top-aligned rather than centred: a wrapped command grows the editor
        // downwards, and centring would float the prompt into the middle of that block and
        // let the chips drift upwards as you type. Top alignment keeps the prompt welded to
        // the command's first line, the way it is in any terminal, and holds the chips still.
        //
        // Prompt and editor then travel together as one child so they share a baseline and
        // can be nudged as a pair; the chips are a separate child and stay where they are.
        let mut command_line = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Start);
        if let Some(shell_prompt) = shell_prompt {
            command_line.add_child(shell_prompt);
        }
        command_line.add_child(Expanded::new(1., input_box).finish());

        let mut input_row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Start);
        input_row.add_child(
            Expanded::new(
                1.,
                Container::new(command_line.finish())
                    .with_padding_top(COMMAND_LINE_TOP_NUDGE)
                    .finish(),
            )
            .finish(),
        );
        let chips_below = if chips_fit_beside_the_command {
            input_row.add_child(Container::new(folder_chip).with_margin_right(8.).finish());
            None
        } else {
            Some(folder_chip)
        };

        // The editor carries bottom padding for the stacked layout it was built for; on one
        // row that lands all the slack under the text and jams it against the top border, so
        // give the row matching space above.
        column.add_child(
            Container::new(input_row.finish())
                .with_padding_top(COMPACT_ROW_TOP_PADDING)
                .finish(),
        );

        // Pinned under the command line, still hugging the right edge so the chips sit
        // where the eye already looks for them.
        if let Some(folder_chip) = chips_below {
            column.add_child(
                Container::new(
                    Flex::row()
                        .with_child(Expanded::new(1., Empty::new().finish()).finish())
                        .with_child(folder_chip)
                        .finish(),
                )
                .with_margin_right(8.)
                .with_margin_top(CHIPS_BELOW_COMMAND_GAP)
                .finish(),
            );
        }

        if matches!(input_mode, InputMode::PinnedToTop) {
            if let Some(banner) = self.render_input_banner(appearance, app, input_mode, false) {
                column.add_child(
                    Container::new(banner)
                        .with_margin_bottom(spacing::UDI_CHIP_MARGIN)
                        .finish(),
                );
            }
        }

        stack.add_child(wrap_input_with_terminal_padding_and_focus_handler(
            self.focus_handle
                .as_ref()
                .is_some_and(|h| h.is_active_session(app)),
            column.finish(),
            false,
        ));

        if let Some(selected_workflow_state) = self.workflows_state.selected_workflow_state.as_ref()
        {
            if selected_workflow_state.should_show_more_info_view {
                add_workflow_info_overlay(
                    &mut stack,
                    selected_workflow_state,
                    self.size_info(app).pane_height_px().as_f32(),
                    menu_positioning,
                );
            }
        }

        let is_focused = self.focus_handle.as_ref().is_none_or(|h| h.is_focused(app));
        if self.is_voltron_open && is_focused {
            add_voltron_overlay(&mut stack, &self.voltron_view, menu_positioning);
        }

        if is_focused {
            add_input_suggestions_overlays(self, &mut stack, appearance, menu_positioning, app);
        }

        if let Some(token_description) = &self.command_x_ray_description {
            add_command_xray_overlay(
                self,
                &mut stack,
                token_description,
                appearance,
                menu_positioning,
                app,
            );
        }

        let drop_target = DropTarget::new(
            SavePosition::new(stack.finish(), &self.status_free_input_save_position_id()).finish(),
            InputDropTargetData::new(self.weak_view_handle.clone()),
        )
        .finish();

        let hoverable_input = Hoverable::new(self.hoverable_handle.clone(), |_| drop_target)
            .on_hover(|is_hovered, ctx, _app, _position| {
                ctx.dispatch_typed_action(InputAction::SetUDIHovered(is_hovered));
            })
            .on_middle_click(|ctx, _app, _position| {
                ctx.dispatch_typed_action(TerminalAction::MiddleClickOnInput)
            })
            .finish();

        let show_block_dividers = *BlockListSettings::as_ref(app).show_block_dividers.value();

        let input = if show_block_dividers {
            Container::new(hoverable_input)
                .with_border(
                    Border::top(1.)
                        .with_border_color(styles::default_border_color(appearance.theme())),
                )
                .finish()
        } else {
            hoverable_input
        };

        let mut column = Flex::column();
        let is_slash_commands = self.suggestions_mode_model.as_ref(app).is_slash_commands();
        let is_conversation_menu = self
            .suggestions_mode_model
            .as_ref(app)
            .is_conversation_menu();
        let is_prompts_menu = self.suggestions_mode_model.as_ref(app).is_prompts_menu();
        let is_skill_menu = self.suggestions_mode_model.as_ref(app).is_skill_menu();
        let is_inline_history_menu = self
            .suggestions_mode_model
            .as_ref(app)
            .is_inline_history_menu();
        let is_repos_menu = self.suggestions_mode_model.as_ref(app).is_repos_menu();
        let hide_menu = self
            .inline_terminal_menu_positioner
            .as_ref(app)
            .should_hide_inline_menu_for_pane_size(app);
        match input_mode {
            InputMode::PinnedToBottom => {
                column.add_children(
                    [
                        if hide_menu {
                            None
                        } else if is_slash_commands {
                            Some(ChildView::new(&self.inline_slash_commands_view).finish())
                        } else if is_prompts_menu {
                            Some(ChildView::new(&self.inline_prompts_menu_view).finish())
                        } else if is_conversation_menu {
                            Some(ChildView::new(&self.inline_conversation_menu_view).finish())
                        } else if FeatureFlag::ListSkills.is_enabled() && is_skill_menu {
                            Some(ChildView::new(&self.inline_skill_selector_view).finish())
                        } else if is_inline_history_menu {
                            Some(ChildView::new(&self.inline_history_menu_view).finish())
                        } else if is_repos_menu {
                            Some(ChildView::new(&self.inline_repos_menu_view).finish())
                        } else {
                            None
                        },
                        Some(ChildView::new(&self.agent_status_view).finish()),
                        Some(input),
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            InputMode::PinnedToTop => {
                column.add_children(
                    [
                        Some(input),
                        Some(ChildView::new(&self.agent_status_view).finish()),
                        if hide_menu {
                            None
                        } else if is_slash_commands {
                            Some(ChildView::new(&self.inline_slash_commands_view).finish())
                        } else if is_prompts_menu {
                            Some(ChildView::new(&self.inline_prompts_menu_view).finish())
                        } else if is_conversation_menu {
                            Some(ChildView::new(&self.inline_conversation_menu_view).finish())
                        } else if FeatureFlag::ListSkills.is_enabled() && is_skill_menu {
                            Some(ChildView::new(&self.inline_skill_selector_view).finish())
                        } else if is_inline_history_menu {
                            Some(ChildView::new(&self.inline_history_menu_view).finish())
                        } else if is_repos_menu {
                            Some(ChildView::new(&self.inline_repos_menu_view).finish())
                        } else {
                            None
                        },
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            InputMode::Waterfall => {
                let should_render_below = self
                    .inline_terminal_menu_positioner
                    .as_ref(app)
                    .should_render_inline_menu_below_input();

                if !hide_menu {
                    if is_slash_commands && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_slash_commands_view).finish());
                    } else if is_prompts_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_prompts_menu_view).finish());
                    } else if is_conversation_menu && !should_render_below {
                        column.add_child(
                            ChildView::new(&self.inline_conversation_menu_view).finish(),
                        );
                    } else if FeatureFlag::ListSkills.is_enabled()
                        && is_skill_menu
                        && !should_render_below
                    {
                        column.add_child(ChildView::new(&self.inline_skill_selector_view).finish());
                    } else if is_inline_history_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_history_menu_view).finish());
                    } else if is_repos_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_repos_menu_view).finish());
                    }
                }

                column.add_children([ChildView::new(&self.agent_status_view).finish(), input]);

                if !hide_menu {
                    if is_slash_commands && should_render_below {
                        column.add_child(ChildView::new(&self.inline_slash_commands_view).finish());
                    } else if is_prompts_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_prompts_menu_view).finish());
                    } else if is_conversation_menu && should_render_below {
                        column.add_child(
                            ChildView::new(&self.inline_conversation_menu_view).finish(),
                        );
                    } else if FeatureFlag::ListSkills.is_enabled()
                        && is_skill_menu
                        && should_render_below
                    {
                        column.add_child(ChildView::new(&self.inline_skill_selector_view).finish());
                    } else if is_inline_history_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_history_menu_view).finish());
                    } else if is_repos_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_repos_menu_view).finish());
                    }
                }
            }
        }

        SavePosition::new(column.finish(), &self.save_position_id()).finish()
    }
}

pub mod styles {
    use pathfinder_color::ColorU;
    use krust_core::ui::theme::KrustTheme;

    use crate::ui_components::blended_colors;

    /// The rule between the blocks above and the input below. `outline()` is a UI-chrome
    /// weight that reads as a hard edge across the full width of a terminal; a 5% blend of
    /// the foreground into the background separates the two areas without drawing the eye.
    pub fn default_border_color(theme: &KrustTheme) -> ColorU {
        blended_colors::neutral_1(theme)
    }
}
