//! Round-trip test for [`LocalHarnessStream`]: begin → apply_messages →
//! complete against the real `BlocklistAIHistoryModel`.
//!
//! Guards the optimistic-root-task initialization handshake: a fresh
//! conversation's root task has no source until a `CreateTask` action upgrades
//! it, and `Task::add_messages` rejects uninitialized tasks. A regression here
//! silently drops the agent's reply — the first `AddMessagesToTask` fails with
//! `TaskNotInitialized` (removing the root task from the store as a side
//! effect), and every subsequent injection and the final completion report
//! `TaskNotFound`.

use ai::agent::{TurnEvent, TurnUsage};
use warp_multi_agent_api as api;
use warpui::{App, EntityId};

use super::LocalHarnessStream;
use crate::ai::agent::conversation::ConversationStatus;
use crate::ai::agent::{AIAgentInput, AIAgentOutputStatus, FinishedAIAgentOutput};
use crate::ai::agent_sdk::driver::harness::turn_events_to_conversation::{
    tool_call_action_result, turn_event_to_messages,
};
use crate::ai::blocklist::history_model::BlocklistAIHistoryModel;
use crate::test_util::settings::initialize_history_persistence_for_tests;

#[test]
fn back_to_back_runs_each_land_their_own_completed_conversation() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        for turn in 0..2 {
            let prompt = format!("prompt {turn}");
            let stream =
                app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, &prompt, ctx));
            let messages = turn_event_to_messages(
                stream.root_task_id(),
                stream.request_id(),
                &TurnEvent::Text {
                    text: format!("reply {turn}"),
                },
                &mut std::collections::HashMap::new(),
            );
            app.update(|ctx| stream.apply_messages(messages, ctx));
            app.update(|ctx| stream.complete(ctx));

            history_model.read(&app, |model, _| {
                let conversation = model
                    .conversation(&stream.conversation_id())
                    .expect("conversation exists");
                assert!(
                    matches!(conversation.status(), ConversationStatus::Success),
                    "run {turn} must complete",
                );
            });
        }
    });
}

#[test]
fn exchanges_carry_the_user_query_as_renderable_input() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let first =
            app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "first prompt", ctx));
        let second = app
            .update(|ctx| {
                LocalHarnessStream::begin_follow_up(
                    terminal_view_id,
                    first.conversation_id(),
                    "second prompt",
                    ctx,
                )
            })
            .expect("follow-up registers on the live conversation");
        assert_eq!(second.conversation_id(), first.conversation_id());

        // The query bubble renders from each exchange's `input`
        // (`inputs_to_render` → `display_user_query`), so both prompts must
        // be present there — a regression here makes the user's own inputs
        // vanish from the transcript.
        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&first.conversation_id())
                .expect("conversation exists");
            let queries: Vec<String> = conversation
                .root_task_exchanges()
                .filter_map(|exchange| exchange.input.iter().find_map(AIAgentInput::user_query))
                .collect();
            assert_eq!(
                queries,
                vec!["first prompt".to_string(), "second prompt".to_string()]
            );

            // The prompts must also live in the task's message list: tasks
            // are what persist, and restoring a conversation rebuilds its
            // query bubbles from the UserQuery messages found there. Without
            // these, history shows responses with no inputs after a restart.
            let task = conversation
                .get_task(conversation.get_root_task_id())
                .expect("root task exists");
            let persisted_queries: Vec<String> = task
                .messages()
                .filter_map(|message| match &message.message {
                    Some(api::message::Message::UserQuery(user_query)) => {
                        Some(user_query.query.clone())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                persisted_queries,
                vec!["first prompt".to_string(), "second prompt".to_string()]
            );
        });
    });
}

#[test]
fn turn_usage_populates_context_meter_and_accumulates_token_totals() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        // Folding usage metadata into the conversation resolves custom
        // endpoint display labels through the LLM preferences singleton,
        // which watches network/auth/workspace state at construction
        // (mirrors conversation_tests' custom-endpoint app setup).
        app.add_singleton_model(|_| crate::server::server_api::ServerApiProvider::new_for_test());
        app.add_singleton_model(|_| crate::network::NetworkStatus::new());
        app.add_singleton_model(crate::workspaces::user_workspaces::UserWorkspaces::default_mock);
        app.add_singleton_model(|_| crate::auth::AuthStateProvider::new_for_test());
        app.add_singleton_model(crate::auth::auth_manager::AuthManager::new_for_test);
        app.add_singleton_model(crate::ai::llms::LLMPreferences::new);

        let stream = app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "prompt", ctx));
        let usage = TurnUsage {
            input_tokens: 10,
            output_tokens: 90,
            cache_read_input_tokens: 30_000,
            cache_creation_input_tokens: 9_900,
            model_id: Some("claude-sonnet-4-5".to_string()),
            context_window: Some(200_000),
            cost_usd: Some(0.05),
            duration_ms: Some(1_000),
        };
        app.update(|ctx| stream.apply_turn_usage(&usage, ctx));

        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&stream.conversation_id())
                .expect("conversation exists");
            // 40_000 context tokens of a 200_000-token window.
            assert!(
                (conversation.context_window_usage() - 0.2).abs() < 1e-6,
                "context fraction: {}",
                conversation.context_window_usage()
            );
            let rows = conversation.token_usage();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].model_id, "claude-sonnet-4-5");
            assert_eq!(rows[0].byok_tokens, 40_000);
        });

        // A second run's usage accumulates into the totals rather than
        // replacing them, while the context fraction stays point-in-time.
        app.update(|ctx| stream.apply_turn_usage(&usage, ctx));
        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&stream.conversation_id())
                .expect("conversation exists");
            assert!((conversation.context_window_usage() - 0.2).abs() < 1e-6);
            assert_eq!(conversation.token_usage()[0].byok_tokens, 80_000);
        });
    });
}

#[test]
fn follow_up_appends_a_second_exchange_to_the_same_conversation() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let first = app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "first", ctx));
        let conversation_id = first.conversation_id();
        let messages = turn_event_to_messages(
            first.root_task_id(),
            first.request_id(),
            &TurnEvent::Text {
                text: "first reply".to_string(),
            },
            &mut std::collections::HashMap::new(),
        );
        app.update(|ctx| first.apply_messages(messages, ctx));
        app.update(|ctx| first.complete(ctx));

        let second = app
            .update(|ctx| {
                LocalHarnessStream::begin_follow_up(
                    terminal_view_id,
                    conversation_id,
                    "second",
                    ctx,
                )
            })
            .expect("conversation still live");
        assert_eq!(
            second.conversation_id(),
            conversation_id,
            "follow-up reuses the conversation"
        );
        let messages = turn_event_to_messages(
            second.root_task_id(),
            second.request_id(),
            &TurnEvent::Text {
                text: "second reply".to_string(),
            },
            &mut std::collections::HashMap::new(),
        );
        app.update(|ctx| second.apply_messages(messages, ctx));
        app.update(|ctx| second.complete(ctx));

        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&conversation_id)
                .expect("conversation exists");
            let exchanges: Vec<_> = conversation.root_task_exchanges().collect();
            assert_eq!(exchanges.len(), 2, "one exchange per prompt");
            for exchange in &exchanges {
                assert!(
                    matches!(exchange.output_status, AIAgentOutputStatus::Finished { .. }),
                    "every exchange completes"
                );
            }
            assert!(matches!(conversation.status(), ConversationStatus::Success));
        });
    });
}

#[test]
fn stream_round_trip_lands_a_completed_exchange_with_output() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let stream = app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "say hi", ctx));

        let messages = turn_event_to_messages(
            stream.root_task_id(),
            stream.request_id(),
            &TurnEvent::Text {
                text: "hello from the harness".to_string(),
            },
            &mut std::collections::HashMap::new(),
        );
        assert!(!messages.is_empty(), "a text event must be renderable");
        app.update(|ctx| stream.apply_messages(messages, ctx));
        app.update(|ctx| stream.complete(ctx));

        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&stream.conversation_id())
                .expect("conversation exists");
            let exchanges: Vec<_> = conversation.root_task_exchanges().collect();
            assert_eq!(exchanges.len(), 1, "exactly one exchange for the run");
            let AIAgentOutputStatus::Finished { finished_output } = &exchanges[0].output_status
            else {
                panic!(
                    "exchange must complete once the stream finishes: {:?}",
                    exchanges[0].output_status
                );
            };
            // The reply must survive the whole pipeline as renderable output —
            // "completed but empty" is exactly the bug where the user sees the
            // busy spinner stop without any reply text.
            let FinishedAIAgentOutput::Success { output } = finished_output else {
                panic!("finished output must be a success");
            };
            let output = output.get();
            assert!(
                output
                    .messages
                    .iter()
                    .any(|message| format!("{message:?}").contains("hello from the harness")),
                "the agent's text must land in the exchange output, got: {:?}",
                output.messages,
            );
            assert!(matches!(conversation.status(), ConversationStatus::Success));
        });
    });
}

#[test]
fn text_deltas_append_to_one_message_without_inventing_line_breaks() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let mut stream =
            app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "stream it", ctx));
        app.update(|ctx| {
            stream.apply_text_delta("alpha beta", ctx);
            stream.apply_text_delta(" gamma\nreal line", ctx);
        });
        app.update(|ctx| stream.complete(ctx));

        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&stream.conversation_id())
                .expect("conversation exists");
            let task = conversation
                .get_task(conversation.get_root_task_id())
                .expect("root task exists");
            let outputs: Vec<&str> = task
                .messages()
                .filter_map(|message| match &message.message {
                    Some(api::message::Message::AgentOutput(output)) => Some(output.text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(outputs, vec!["alpha beta gamma\nreal line"]);
        });
    });
}

#[test]
fn tool_result_split_lands_a_continuation_exchange_in_the_same_conversation() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let stream = app.update(|ctx| LocalHarnessStream::begin(terminal_view_id, "list it", ctx));
        let conversation_id = stream.conversation_id();

        // The exchange output ends with a shell tool call...
        let mut shell_commands = std::collections::HashMap::new();
        for event in [
            TurnEvent::Text {
                text: "let me look".to_string(),
            },
            TurnEvent::ToolCallStarted {
                call_id: "t1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({ "command": "ls" }),
            },
            TurnEvent::ToolCallResult {
                call_id: "t1".to_string(),
                outcome: ai::agent::ToolCallOutcome::Success(
                    serde_json::json!({ "stdout": "file1" }),
                ),
            },
        ] {
            let messages = turn_event_to_messages(
                stream.root_task_id(),
                stream.request_id(),
                &event,
                &mut shell_commands,
            );
            app.update(|ctx| stream.apply_messages(messages, ctx));
        }

        // ...then the run continues in a fresh exchange whose input is the
        // tool's result (the cloud turn shape).
        let result = tool_call_action_result(
            stream.root_task_id(),
            "t1",
            &ai::agent::ToolCallOutcome::Success(serde_json::json!({ "stdout": "file1" })),
        );
        let stream = app.update(|ctx| stream.continue_after_tool_results(vec![result], ctx));
        assert_eq!(
            stream.conversation_id(),
            conversation_id,
            "continuation stays in the conversation"
        );

        let messages = turn_event_to_messages(
            stream.root_task_id(),
            stream.request_id(),
            &TurnEvent::Text {
                text: "file1 is all there is".to_string(),
            },
            &mut shell_commands,
        );
        app.update(|ctx| stream.apply_messages(messages, ctx));
        app.update(|ctx| stream.complete(ctx));

        history_model.read(&app, |model, _| {
            let conversation = model
                .conversation(&conversation_id)
                .expect("conversation exists");
            let exchanges: Vec<_> = conversation.root_task_exchanges().collect();
            assert_eq!(exchanges.len(), 2, "the tool result splits the exchange");
            for exchange in &exchanges {
                assert!(
                    matches!(exchange.output_status, AIAgentOutputStatus::Finished { .. }),
                    "both exchanges complete"
                );
            }
            // The continuation's input is the action result, not a user query
            // (no spurious query bubble).
            assert!(
                exchanges[1]
                    .input
                    .iter()
                    .all(|input| matches!(input, AIAgentInput::ActionResult { .. })),
                "continuation input is the tool result, got: {:?}",
                exchanges[1].input,
            );
            assert!(matches!(conversation.status(), ConversationStatus::Success));
        });
    });
}
