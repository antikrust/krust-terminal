use std::rc::Rc;

use krust_core::settings::Setting as _;
use krustui::{App, AppContext, SingletonEntity, ViewContext};

use super::super::{AIBlockMetadata, RichContentMetadata, RichContentType};
use super::*;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{AIAgentInput, ServerOutputId, UserQueryMode};
use crate::ai::blocklist::agent_view::AgentViewEntryOrigin;
use crate::ai::blocklist::block::cli_controller::UserTakeOverReason;
use crate::ai::blocklist::model::{
    AIBlockModel, AIBlockOutputStatus, AIRequestType, OutputStatusUpdateCallback,
};
use crate::ai::blocklist::{AIBlock, ClientIdentifiers};
use crate::ai::llms::LLMId;
use crate::features::FeatureFlag;
use crate::settings::AISettings;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
    CLIAgentSessionsModel,
};
use crate::terminal::model::ansi::{BootstrappedValue, Handler as _, InitShellValue};
use crate::terminal::shared_session::{SharedSessionSource, SharedSessionStatus};
use crate::terminal::CLIAgent;
use crate::test_util::add_window_with_terminal;
use crate::test_util::terminal::initialize_app_for_terminal_view;

struct PendingAIBlockModel {
    conversation_id: AIConversationId,
    input: Vec<AIAgentInput>,
    model_id: LLMId,
}

impl PendingAIBlockModel {
    fn new(conversation_id: AIConversationId, input: Vec<AIAgentInput>) -> Self {
        Self {
            conversation_id,
            input,
            model_id: LLMId::from("fake-llm"),
        }
    }
}

impl AIBlockModel for PendingAIBlockModel {
    type View = AIBlock;

    fn status(&self, _app: &AppContext) -> AIBlockOutputStatus {
        AIBlockOutputStatus::Pending
    }

    fn server_output_id(&self, _app: &AppContext) -> Option<ServerOutputId> {
        None
    }

    fn model_id(&self, _app: &AppContext) -> Option<LLMId> {
        None
    }

    fn base_model<'a>(&'a self, _app: &'a AppContext) -> Option<&'a LLMId> {
        Some(&self.model_id)
    }

    fn inputs_to_render<'a>(&'a self, _app: &'a AppContext) -> &'a [AIAgentInput] {
        &self.input
    }

    fn conversation_id(&self, _app: &AppContext) -> Option<AIConversationId> {
        Some(self.conversation_id)
    }

    fn on_updated_output(
        &self,
        _callback: OutputStatusUpdateCallback<AIBlock>,
        _ctx: &mut ViewContext<AIBlock>,
    ) {
    }

    fn request_type(&self, _app: &AppContext) -> AIRequestType {
        AIRequestType::Active
    }
}

fn simulate_user_started_long_running_command(view: &mut TerminalView) {
    {
        let mut model = view.model.lock();
        model.init_shell(InitShellValue {
            session_id: 0.into(),
            shell: "zsh".to_owned(),
            ..Default::default()
        });
        model.bootstrapped(BootstrappedValue {
            shell: "zsh".to_owned(),
            ..Default::default()
        });
        model.simulate_long_running_block("ssh localhost", "Password:");
    }
}

fn transition_to_user_handoff_state(
    view: &mut TerminalView,
    reason: UserTakeOverReason,
    ctx: &mut ViewContext<TerminalView>,
) -> AIConversationId {
    let conversation_id = view.agent_view_controller().update(ctx, |controller, ctx| {
        controller
            .try_enter_inline_agent_view(None, AgentViewEntryOrigin::LongRunningCommand, ctx)
            .expect("inline agent view should create a conversation")
    });
    view.model
        .lock()
        .block_list_mut()
        .active_block_mut()
        .set_is_agent_tagged_in(true);

    let task_id = TaskId::new("test-task".to_owned());
    view.model
        .lock()
        .block_list_mut()
        .active_block_mut()
        .set_agent_interaction_mode_for_agent_monitored_command(&task_id, conversation_id)
        .expect("tagged-in command should transition to agent-monitored");

    view.cli_subagent_controller.update(ctx, |controller, ctx| {
        controller.switch_control_to_user(reason, ctx);
    });

    conversation_id
}

fn insert_pending_ai_block(
    view: &mut TerminalView,
    conversation_id: AIConversationId,
    ctx: &mut ViewContext<TerminalView>,
) {
    let ai_block_model = Rc::new(PendingAIBlockModel::new(
        conversation_id,
        vec![AIAgentInput::UserQuery {
            query: "help with this running command".to_owned(),
            context: vec![].into(),
            static_query_type: None,
            referenced_attachments: Default::default(),
            user_query_mode: UserQueryMode::default(),
            running_command: None,
            intended_agent: None,
        }],
    ));
    let ai_block = ctx.add_typed_action_view(|ctx| {
        AIBlock::new(
            ai_block_model.clone(),
            view.model.clone(),
            ClientIdentifiers {
                client_exchange_id: Default::default(),
                conversation_id,
                response_stream_id: None,
            },
            view.ai_controller.clone(),
            view.get_relevant_files_controller.clone(),
            None,
            None,
            view.ai_action_model.clone(),
            view.ai_context_model.clone(),
            view.find_model.clone(),
            view.active_session.clone(),
            &view.cli_subagent_controller,
            &view.model_events_handle,
            view.agent_view_controller.clone(),
            view.ambient_agent_view_model.clone(),
            view.view_handle.clone(),
            view.id(),
            ctx,
        )
    });

    view.insert_rich_content(
        Some(RichContentType::AIBlock),
        ai_block.clone(),
        Some(RichContentMetadata::AIBlock(AIBlockMetadata {
            exchange_id: Default::default(),
            conversation_id,
            ai_block_handle: ai_block,
        })),
        RichContentInsertionPosition::Append {
            insert_below_long_running_block: false,
        },
        ctx,
    );
}

#[test]
fn use_agent_footer_renders_for_manual_handoff_even_when_user_command_footer_setting_disabled() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        FeatureFlag::AgentView.set_enabled(true);
        AISettings::handle(&app).update(&mut app, |settings, ctx| {
            let _ = settings
                .should_render_use_agent_footer_for_user_commands
                .set_value(false, ctx);
        });

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_long_running_command(view);

            view.maybe_show_use_agent_footer_in_blocklist(ctx);
            {
                let model = view.model.lock();
                assert!(!view.should_render_use_agent_footer(&model, ctx));
                let active_block_index = model.block_list().active_block_index();
                assert!(model
                    .block_list()
                    .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
                    .is_none());
            }

            transition_to_user_handoff_state(view, UserTakeOverReason::Manual, ctx);

            view.maybe_show_use_agent_footer_in_blocklist(ctx);
            let model = view.model.lock();
            assert!(view.should_render_use_agent_footer(&model, ctx));
            let active_block_index = model.block_list().active_block_index();
            let rendered_footer_view_id = model
                .block_list()
                .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
                .map(|(_, item)| item.view_id);
            assert_eq!(rendered_footer_view_id, Some(view.use_agent_footer.id()));
        });
    })
}

#[test]
fn use_agent_footer_renders_for_manual_handoff_when_unfinished_ai_block_remains() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        FeatureFlag::AgentView.set_enabled(true);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_long_running_command(view);

            let conversation_id = view.agent_view_controller().update(ctx, |controller, ctx| {
                controller
                    .try_enter_inline_agent_view(
                        None,
                        AgentViewEntryOrigin::LongRunningCommand,
                        ctx,
                    )
                    .expect("inline agent view should create a conversation")
            });
            view.model
                .lock()
                .block_list_mut()
                .active_block_mut()
                .set_is_agent_tagged_in(true);
            let task_id = TaskId::new("test-task".to_owned());
            view.model
                .lock()
                .block_list_mut()
                .active_block_mut()
                .set_agent_interaction_mode_for_agent_monitored_command(&task_id, conversation_id)
                .expect("tagged-in command should transition to agent-monitored");

            insert_pending_ai_block(view, conversation_id, ctx);
            assert!(view.active_ai_block(ctx).is_some());

            view.cli_subagent_controller.update(ctx, |controller, ctx| {
                controller.switch_control_to_user(UserTakeOverReason::Manual, ctx);
            });
        });

        terminal.read(&app, |view, ctx| {
            let model = view.model.lock();
            assert!(view.should_render_use_agent_footer(&model, ctx));
            let active_block_index = model.block_list().active_block_index();
            let rendered_footer_view_id = model
                .block_list()
                .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
                .map(|(_, item)| item.view_id);
            assert_eq!(rendered_footer_view_id, Some(view.use_agent_footer.id()));
        });
    })
}

/// During the setup phase of a cloud agent (ambient) shared session — LRCs
/// running before any CLI agent has started — the use-agent footer must stay
/// hidden.
#[test]
fn use_agent_footer_hidden_during_cloud_agent_setup_lrc() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_long_running_command(view);

            // Cloud agent setup phase: ambient source type set, LRC running,
            // NO CLIAgentSession registered yet.
            view.model
                .lock()
                .set_shared_session_source(SharedSessionSource::ambient_agent(None));
            assert!(view.model.lock().is_shared_ambient_agent_session());
            assert!(
                CLIAgentSessionsModel::as_ref(ctx)
                    .session(view.id())
                    .is_none(),
                "precondition: no CLI agent session yet",
            );

            view.maybe_show_use_agent_footer_in_blocklist(ctx);

            let model = view.model.lock();
            assert!(
                !view.should_render_use_agent_footer(&model, ctx),
                "footer should be hidden during cloud agent setup LRCs",
            );
            let active_block_index = model.block_list().active_block_index();
            assert!(
                model
                    .block_list()
                    .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
                    .is_none(),
                "footer rich content should not be in the blocklist during cloud setup",
            );
        });
    })
}

/// When viewing a shared cloud-agent (ambient agent) session whose sharer is
/// running a CLI agent, the CLI agent footer should still render.
#[test]
fn cli_agent_footer_renders_for_viewer_of_shared_cloud_agent_session() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_long_running_command(view);

            // Mark the model as a shared ambient (cloud) agent session, mirroring
            // what the viewer's terminal manager does on `JoinedSuccessfully`.
            view.model
                .lock()
                .set_shared_session_source(SharedSessionSource::ambient_agent(None));
            assert!(view.model.lock().is_shared_ambient_agent_session());

            // Inject a CLI agent session as `apply_cli_agent_state_update` would on
            // the viewer when the sharer reports an active CLI agent.
            let view_id = view.id();
            CLIAgentSessionsModel::handle(ctx).update(ctx, |sessions, ctx| {
                sessions.set_session(
                    view_id,
                    CLIAgentSession {
                        agent: CLIAgent::Claude,
                        status: CLIAgentSessionStatus::InProgress,
                        session_context: CLIAgentSessionContext::default(),
                        input_state: CLIAgentInputState::Closed,
                        listener: None,
                        plugin_version: None,
                        remote_host: None,
                        draft_text: None,
                        custom_command_prefix: None,
                        received_rich_notification: false,
                        should_auto_toggle_input: false,
                    },
                    ctx,
                );
            });

            view.maybe_show_use_agent_footer_in_blocklist(ctx);

            let model = view.model.lock();
            assert!(
                view.should_render_use_agent_footer(&model, ctx),
                "footer should render for viewer of shared cloud agent session with CLI agent",
            );
            let active_block_index = model.block_list().active_block_index();
            let rendered_footer_view_id = model
                .block_list()
                .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
                .map(|(_, item)| item.view_id);
            assert_eq!(rendered_footer_view_id, Some(view.use_agent_footer.id()));
        });
    })
}

/// Like [`simulate_user_started_long_running_command`], but the command runs
/// in a block that started after the shell bootstrapped, as a real one does,
/// so the agent can be tagged in to it.
fn simulate_user_started_command_after_bootstrap(view: &mut TerminalView) {
    simulate_user_started_long_running_command(view);
    let mut model = view.model.lock();
    model.finish_block();
    model.simulate_long_running_block("bash", "bash-3.2$ ");
    assert!(model
        .block_list()
        .active_block()
        .is_eligible_to_tag_in_agent());
}

/// The footer's place in the blocklist, if it is there: right after the
/// active (running) block.
fn rendered_use_agent_footer(view: &TerminalView) -> Option<EntityId> {
    let model = view.model.lock();
    let active_block_index = model.block_list().active_block_index();
    model
        .block_list()
        .last_non_hidden_rich_content_block_after_block(Some(active_block_index))
        .map(|(_, item)| item.view_id)
}

/// "Use agent" on a running command in a plain terminal: no local
/// conversation is on screen, so nothing is tagged in or opened, and the
/// footer (which the subshell variant hides on click) is back to try again.
#[test]
fn use_agent_on_running_command_explains_instead_of_tagging_in() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        app.add_singleton_model(|_| ToastStack);
        let _agent_view = FeatureFlag::AgentView.override_enabled(true);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_command_after_bootstrap(view);
            view.maybe_show_use_agent_footer_in_blocklist(ctx);
            assert_eq!(
                rendered_use_agent_footer(view),
                Some(view.use_agent_footer.id())
            );
            view.hide_use_agent_footer_in_blocklist(ctx);

            view.handle_action(&TerminalAction::SetInputModeAgent, ctx);

            assert!(!view.agent_view_controller().as_ref(ctx).is_active());
            assert!(!view
                .model
                .lock()
                .block_list()
                .active_block()
                .is_agent_tagged_in());
            assert_eq!(
                rendered_use_agent_footer(view),
                Some(view.use_agent_footer.id())
            );
        });
    })
}

/// In an agent tab the fullscreen agent view is already open when the
/// command starts. "Use agent" leaves it on its conversation instead of
/// trying to open an inline one over it.
#[test]
fn use_agent_on_running_command_in_agent_view_stays_on_its_conversation() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        app.add_singleton_model(|_| ToastStack);
        let _agent_view = FeatureFlag::AgentView.override_enabled(true);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            let conversation_id = view.agent_view_controller().update(ctx, |controller, ctx| {
                controller
                    .try_enter_agent_view(
                        None,
                        AgentViewEntryOrigin::Input {
                            was_prompt_autodetected: false,
                        },
                        ctx,
                    )
                    .expect("agent view should open while no command runs")
            });
            simulate_user_started_command_after_bootstrap(view);

            view.handle_action(&TerminalAction::SetInputModeAgent, ctx);

            let agent_view_state = view.agent_view_controller().as_ref(ctx).agent_view_state();
            assert!(agent_view_state.is_fullscreen());
            assert_eq!(
                agent_view_state.active_conversation_id(),
                Some(conversation_id)
            );
            assert!(!view
                .model
                .lock()
                .block_list()
                .active_block()
                .is_agent_tagged_in());
        });
    })
}

/// A shared session viewer's agent prompts go to the sharer, so "Use agent"
/// still tags the agent in for them.
#[test]
fn use_agent_on_running_command_as_viewer_tags_in() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _agent_view = FeatureFlag::AgentView.override_enabled(true);

        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |view, ctx| {
            simulate_user_started_command_after_bootstrap(view);
            view.model
                .lock()
                .set_shared_session_status(SharedSessionStatus::executor());

            view.handle_action(&TerminalAction::SetInputModeAgent, ctx);

            assert!(view.agent_view_controller().as_ref(ctx).is_inline());
            assert!(view
                .model
                .lock()
                .block_list()
                .active_block()
                .is_agent_tagged_in());
        });
    })
}

#[test]
fn local_agent_is_asked_about_running_command_only_in_idle_local_conversation() {
    use LocalHarnessKind::{Claude, Codex, Ollama};
    let can_ask = can_ask_local_agent_about_running_command;

    // Nothing local on screen to continue.
    assert!(!can_ask(None, false, true));
    // Still answering.
    assert!(!can_ask(Some(Claude), true, true));
    assert!(can_ask(Some(Claude), false, false));
    assert!(can_ask(Some(Codex), false, false));
    assert!(can_ask(Some(Ollama), false, true));
    // Ollama switched off, or working through runbooks.
    assert!(!can_ask(Some(Ollama), false, false));
}

#[test]
fn running_command_help_prompt_shows_command_and_tidied_screen() {
    assert_eq!(
        running_command_help_prompt(" bash ", "\n\nbash-3.2$ ls   \nfoo  bar\nbash-3.2$ \n\n"),
        "`bash` is still running in my terminal and I'm stuck in it. You can't type into it \
         for me, so tell me in a few sentences what is going on and exactly what to type or \
         press to get out of it or carry on. You don't need to run anything.\n\n\
         Its screen right now:\n\n```\nbash-3.2$ ls\nfoo  bar\nbash-3.2$\n```"
    );
}

#[test]
fn running_command_help_prompt_says_when_nothing_was_printed() {
    let prompt = running_command_help_prompt("sleep 100", "  \n\n ");
    assert!(prompt.starts_with("`sleep 100` is still running in my terminal"));
    assert!(prompt.ends_with("You don't need to run anything.\n\nIt hasn't printed anything yet."));
}

#[test]
fn running_command_help_prompt_quotes_backticks_safely() {
    let prompt = running_command_help_prompt("echo `date`", "```rust\nfn main() {}\n```");
    assert!(prompt.starts_with("`` echo `date` `` is still running"));
    assert!(prompt.ends_with("\n\n````\n```rust\nfn main() {}\n```\n````"));
}

#[test]
fn tidy_screen_keeps_whole_lines_from_the_end_of_a_long_screen() {
    let filler = "x".repeat(99);
    let screen = (0..100)
        .map(|row| format!("{row:03}{filler}"))
        .collect::<Vec<_>>()
        .join("\n");

    let tidy = tidy_screen(&screen);

    assert!(tidy.len() <= RUNNING_COMMAND_SCREEN_MAX_CHARS);
    assert!(tidy.ends_with(&format!("099{filler}")));
    assert!(tidy.lines().all(|line| line.len() == 102));
}

#[test]
fn tidy_screen_cuts_a_long_line_on_a_char_boundary() {
    let tidy = tidy_screen(&"€".repeat(2000));

    assert!(tidy.len() <= RUNNING_COMMAND_SCREEN_MAX_CHARS);
    assert!(!tidy.is_empty());
    assert!(tidy.chars().all(|c| c == '€'));
}
