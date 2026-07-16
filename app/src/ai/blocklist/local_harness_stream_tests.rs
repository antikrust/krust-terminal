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

use ai::agent::TurnEvent;
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
