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
use warpui::{App, EntityId};

use super::LocalHarnessStream;
use crate::ai::agent::conversation::ConversationStatus;
use crate::ai::agent::{AIAgentOutputStatus, FinishedAIAgentOutput};
use crate::ai::agent_sdk::driver::harness::turn_events_to_conversation::turn_event_to_messages;
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
        );
        app.update(|ctx| first.apply_messages(messages, ctx));
        app.update(|ctx| first.complete(ctx));

        let second = app
            .update(|ctx| {
                LocalHarnessStream::begin_follow_up(terminal_view_id, conversation_id, "second", ctx)
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
