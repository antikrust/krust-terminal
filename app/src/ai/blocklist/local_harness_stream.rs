//! Injects a local CLI harness run (e.g. streaming Claude Code) into the agent
//! conversation UI as a live, incrementally-updating exchange — the same
//! rendering path Krust's own agent uses.
//!
//! Lives inside `blocklist` so it can reach
//! [`BlocklistAIHistoryModel::update_conversation_for_new_request_input`]
//! (which is `pub(super)`), then drives the public streaming methods
//! (`apply_client_actions`, `mark_response_stream_completed_successfully`) as
//! output arrives. Unlike the shared-session path
//! (`controller::shared_session`), this does NOT mark the conversation
//! view-only or as "viewing a shared session" — the user owns and drives it.

use std::collections::HashMap;

use ai::agent::{TurnEvent, TurnUsage};
use ai::skills::SkillPathOrigin;
use chrono::Local;
use prost_types::FieldMask;
use krust_multi_agent_api as api;
use krustui::{AppContext, EntityId, SingletonEntity as _};

use super::controller::response_stream::ResponseStreamId;
use super::controller::RequestInput;
use super::history_model::BlocklistAIHistoryModel;
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{AIAgentInput, UserQueryMode};
use crate::ai::agent_sdk::driver::harness::turn_events_to_conversation::{
    turn_event_to_messages, user_query_message,
};
use crate::ai::llms::LLMId;

/// A handle to one in-progress local-harness exchange. Created with
/// [`begin`](Self::begin) when a run starts; [`apply_messages`](Self::apply_messages)
/// is called as events stream in; [`complete`](Self::complete) closes it.
pub(crate) struct LocalHarnessStream {
    conversation_id: AIConversationId,
    stream_id: ResponseStreamId,
    root_task_id: TaskId,
    request_id: String,
    terminal_surface_id: EntityId,
    skill_path_origin: SkillPathOrigin,
    /// Stable message receiving exact token deltas for this exchange. The
    /// server protocol's append action updates this message in place, so an
    /// Ollama token never becomes a separate line or output block.
    streaming_text_message_id: Option<String>,
    /// Assistant text rendered so far in the current turn, used to drop a
    /// failed-turn error that merely repeats it (see [`messages_for_event`]).
    /// Cleared at each turn boundary.
    shown_texts: Vec<String>,
    /// Stable message receiving streamed reasoning for the current thinking
    /// run — the reasoning twin of [`Self::streaming_text_message_id`].
    /// Without it every thinking delta becomes its own collapsible
    /// "Thinking" section (qwen3 streams reasoning word by word). Reset by
    /// any non-thinking event so a post-tool thinking run opens a new
    /// section.
    streaming_reasoning_message_id: Option<String>,
}

impl LocalHarnessStream {
    /// Starts a fresh conversation on `terminal_surface_id`, registers an
    /// exchange for the user's `prompt`, and returns a handle for streaming the
    /// response into it.
    pub(crate) fn begin(terminal_surface_id: EntityId, prompt: &str, ctx: &mut AppContext) -> Self {
        let history = BlocklistAIHistoryModel::handle(ctx);

        let conversation_id = history.update(ctx, |history, ctx| {
            // (autoexecute_override, viewing_shared_session, cli_agent_transcript)
            // are all false: this is a normal, user-owned local conversation.
            history.start_new_conversation(terminal_surface_id, false, false, false, ctx)
        });

        let root_task_id = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .map(|conversation| conversation.get_root_task_id().clone())
            .unwrap_or_else(|| TaskId::new(String::new()));

        let request_id = uuid::Uuid::new_v4().to_string();
        let stream_init = api::response_event::StreamInit {
            request_id: request_id.clone(),
            conversation_id: String::new(),
            run_id: String::new(),
        };
        let stream_id = ResponseStreamId::for_shared_session(&stream_init);

        let this = Self {
            conversation_id,
            stream_id,
            root_task_id,
            request_id,
            terminal_surface_id,
            skill_path_origin: SkillPathOrigin::Local,
            streaming_text_message_id: None,
            shown_texts: Vec::new(),
            streaming_reasoning_message_id: None,
        };

        let request_input = this.request_input(prompt);
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.update_conversation_for_new_request_input(
                request_input,
                this.stream_id.clone(),
                terminal_surface_id,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to register exchange: {error}");
            }

            // Give the exchange a live output to stream into — on the Oz path
            // this happens when the server's `StreamInit` arrives. Without it,
            // attaching messages fails with `OutputNeverInitialized`.
            history.initialize_output_for_response_stream(
                &this.stream_id,
                this.conversation_id,
                terminal_surface_id,
                stream_init,
                ctx,
            );

            // A fresh conversation's root task is optimistic (`source: None`),
            // and `Task::add_messages` rejects optimistic tasks with
            // `TaskNotInitialized` — worse, the `AddMessagesToTask` handler
            // removes the task from the store before adding and does not
            // restore it on failure, wedging the conversation. In the cloud
            // flow the server's first client action is a `CreateTask` that
            // upgrades the root task in place; mimic that here, reusing the
            // root task's id so the exchange registered above stays bound.
            let create_root_task = api::ClientAction {
                action: Some(api::client_action::Action::CreateTask(
                    api::client_action::CreateTask {
                        task: Some(api::Task {
                            id: this.root_task_id.to_string(),
                            ..Default::default()
                        }),
                    },
                )),
            };
            if let Err(error) = history.apply_client_actions(
                &this.stream_id,
                vec![create_root_task],
                this.conversation_id,
                terminal_surface_id,
                &this.skill_path_origin,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to initialize root task: {error}");
            }

            // Mirror the cloud controller's post-send bookkeeping: the surface's
            // "active conversation" pointer drives pane titles, the details
            // panel, and anything else that follows the most recent exchange.
            history.mark_active_conversation_id(this.conversation_id, terminal_surface_id, ctx);
        });

        // Echo the prompt into the task the way the cloud server does. The
        // live exchange already carries it as input, but tasks are what
        // persist — without this message a restored conversation renders
        // responses with no query bubbles.
        this.apply_messages(
            vec![user_query_message(
                this.root_task_id(),
                this.request_id(),
                prompt,
            )],
            ctx,
        );

        this
    }

    /// Registers a follow-up exchange on an existing local-harness
    /// conversation and returns a handle for streaming the response into it.
    ///
    /// Unlike [`begin`](Self::begin) this reuses the conversation (so the
    /// whole thread stays on screen) and skips the `CreateTask` upgrade —
    /// the root task was already upgraded by the first run, and re-upgrading
    /// a server-backed task fails (losing the task as a side effect).
    /// Returns `None` if the conversation no longer exists; callers should
    /// fall back to starting a fresh one.
    pub(crate) fn begin_follow_up(
        terminal_surface_id: EntityId,
        conversation_id: AIConversationId,
        prompt: &str,
        ctx: &mut AppContext,
    ) -> Option<Self> {
        let history = BlocklistAIHistoryModel::handle(ctx);

        let root_task_id = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .map(|conversation| conversation.get_root_task_id().clone())?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let stream_init = api::response_event::StreamInit {
            request_id: request_id.clone(),
            conversation_id: String::new(),
            run_id: String::new(),
        };
        let stream_id = ResponseStreamId::for_shared_session(&stream_init);

        let this = Self {
            conversation_id,
            stream_id,
            root_task_id,
            request_id,
            terminal_surface_id,
            skill_path_origin: SkillPathOrigin::Local,
            streaming_text_message_id: None,
            shown_texts: Vec::new(),
            streaming_reasoning_message_id: None,
        };

        let request_input = this.request_input(prompt);
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.update_conversation_for_new_request_input(
                request_input,
                this.stream_id.clone(),
                terminal_surface_id,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to register follow-up exchange: {error}");
            }

            history.initialize_output_for_response_stream(
                &this.stream_id,
                this.conversation_id,
                terminal_surface_id,
                stream_init,
                ctx,
            );

            // Follow-ups re-enter progress; nothing server-side flips the
            // status back for local runs, and the input area's "Sending..."
            // shimmer keys off it.
            history.mark_conversation_in_progress(this.conversation_id, terminal_surface_id, ctx);

            history.mark_active_conversation_id(this.conversation_id, terminal_surface_id, ctx);
        });

        // Persist the follow-up prompt into the task, matching `begin`.
        this.apply_messages(
            vec![user_query_message(
                this.root_task_id(),
                this.request_id(),
                prompt,
            )],
            ctx,
        );

        Some(this)
    }

    /// The conversation this stream renders into.
    pub(crate) fn conversation_id(&self) -> AIConversationId {
        self.conversation_id
    }

    /// The task id new messages should be attached to.
    pub(crate) fn root_task_id(&self) -> &str {
        &self.root_task_id
    }

    /// The request id to stamp on messages so they associate with this exchange.
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Converts one streamed event to renderable messages, dropping a
    /// failed-turn error that merely repeats text the harness already
    /// surfaced. Both claude and codex report a usage/session limit as a
    /// normal assistant message *and* a failed turn, which would otherwise
    /// render the same text twice (a plain line plus an error box).
    ///
    /// The genuine invisible-failure case — a run that fails before producing
    /// any output — still renders, since nothing preceded the error.
    pub(crate) fn messages_for_event(
        &mut self,
        event: &TurnEvent,
        shell_commands: &mut HashMap<String, String>,
    ) -> Vec<api::Message> {
        // Any non-delta event ends the current streaming thinking section;
        // the next thinking delta starts a fresh one (e.g. after a tool
        // call). Thinking itself is routed to `apply_thinking`, not here.
        self.streaming_reasoning_message_id = None;
        // Remember assistant text shown so far this turn.
        if let TurnEvent::Text { text } = event {
            self.shown_texts.push(text.clone());
        }
        // A failed turn whose error just repeats already-shown text is a
        // duplicate: render nothing (the turn still ends with error status).
        if let TurnEvent::TurnCompleted {
            is_error: true,
            error_message: Some(message),
            ..
        } = event
        {
            let needle = message.trim();
            let duplicate =
                !needle.is_empty() && self.shown_texts.iter().any(|shown| shown.contains(needle));
            if duplicate {
                self.shown_texts.clear();
                return Vec::new();
            }
        }
        let messages = turn_event_to_messages(
            self.root_task_id(),
            self.request_id(),
            event,
            shell_commands,
        );
        // A completed turn (success or error) closes the dedup window.
        if matches!(event, TurnEvent::TurnCompleted { .. }) {
            self.shown_texts.clear();
        }
        messages
    }

    /// Appends `messages` to the exchange as a new streaming chunk; the UI
    /// re-renders the block in place. No-op for an empty batch.
    pub(crate) fn apply_messages(&self, messages: Vec<api::Message>, ctx: &mut AppContext) {
        if messages.is_empty() {
            return;
        }
        let history = BlocklistAIHistoryModel::handle(ctx);
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.apply_client_actions(
                &self.stream_id,
                vec![api::ClientAction {
                    action: Some(api::client_action::Action::AddMessagesToTask(
                        api::client_action::AddMessagesToTask {
                            task_id: self.root_task_id.to_string(),
                            messages,
                        },
                    )),
                }],
                self.conversation_id,
                self.terminal_surface_id,
                &self.skill_path_origin,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to inject harness messages: {error}");
            }
        });
    }

    /// Appends an exact model delta to one live assistant message. The first
    /// delta creates the message; later deltas use Krust's native protobuf
    /// append action and therefore preserve only newlines the model actually
    /// emitted.
    pub(crate) fn apply_text_delta(&mut self, text: &str, ctx: &mut AppContext) {
        if text.is_empty() {
            return;
        }

        // Answer text ends any streaming thinking section.
        self.streaming_reasoning_message_id = None;
        let is_first_delta = self.streaming_text_message_id.is_none();
        let message_id = self
            .streaming_text_message_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        let message = api::Message {
            id: message_id,
            task_id: self.root_task_id.to_string(),
            request_id: self.request_id.clone(),
            message: Some(api::message::Message::AgentOutput(
                api::message::AgentOutput {
                    text: text.to_string(),
                },
            )),
            ..Default::default()
        };

        let action = if is_first_delta {
            api::client_action::Action::AddMessagesToTask(api::client_action::AddMessagesToTask {
                task_id: self.root_task_id.to_string(),
                messages: vec![message],
            })
        } else {
            api::client_action::Action::AppendToMessageContent(
                api::client_action::AppendToMessageContent {
                    task_id: self.root_task_id.to_string(),
                    message: Some(message),
                    mask: Some(FieldMask {
                        // Field-mask paths address proto FIELDS; oneof names
                        // are not fields. `message.…` would silently no-op
                        // (unknown segments are backwards-compat no-ops in
                        // field_mask::apply_path), losing every delta after
                        // the first.
                        paths: vec!["agent_output.text".to_string()],
                    }),
                },
            )
        };

        let history = BlocklistAIHistoryModel::handle(ctx);
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.apply_client_actions(
                &self.stream_id,
                vec![api::ClientAction {
                    action: Some(action),
                }],
                self.conversation_id,
                self.terminal_surface_id,
                &self.skill_path_origin,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to append text delta: {error}");
            }
        });
    }

    /// Streams a reasoning delta into one stable, growing "Thinking" section
    /// — the reasoning twin of [`Self::apply_text_delta`]. Reasoning models
    /// (qwen3, deepseek-r1) stream their chain of thought word by word;
    /// making each delta its own message rendered a stack of one-word
    /// "Thinking" collapsibles.
    pub(crate) fn apply_thinking(&mut self, text: &str, ctx: &mut AppContext) {
        if text.is_empty() {
            return;
        }

        let is_first_delta = self.streaming_reasoning_message_id.is_none();
        let message_id = self
            .streaming_reasoning_message_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        let message = api::Message {
            id: message_id,
            task_id: self.root_task_id.to_string(),
            request_id: self.request_id.clone(),
            message: Some(api::message::Message::AgentReasoning(
                api::message::AgentReasoning {
                    reasoning: text.to_string(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };

        let action = if is_first_delta {
            api::client_action::Action::AddMessagesToTask(api::client_action::AddMessagesToTask {
                task_id: self.root_task_id.to_string(),
                messages: vec![message],
            })
        } else {
            api::client_action::Action::AppendToMessageContent(
                api::client_action::AppendToMessageContent {
                    task_id: self.root_task_id.to_string(),
                    message: Some(message),
                    mask: Some(FieldMask {
                        // Proto field path — see the note on the text mask in
                        // `apply_text_delta`.
                        paths: vec!["agent_reasoning.reasoning".to_string()],
                    }),
                },
            )
        };

        let history = BlocklistAIHistoryModel::handle(ctx);
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.apply_client_actions(
                &self.stream_id,
                vec![api::ClientAction {
                    action: Some(action),
                }],
                self.conversation_id,
                self.terminal_surface_id,
                &self.skill_path_origin,
                ctx,
            ) {
                log::warn!("local_harness_stream: failed to append thinking delta: {error}");
            }
        });
    }

    /// Folds a run's final token usage into the conversation so the usage
    /// footer and its context-window meter work for local runs. On the Oz
    /// path the server precomputes all of this and ships it in
    /// `StreamFinished`; here we accumulate per-model totals across the
    /// conversation's runs and derive the context fraction ourselves.
    // `ConversationUsageMetadata` construction requires its deprecated
    // `token_usage` field (exhaustive struct literal).
    #[allow(deprecated)]
    pub(crate) fn apply_turn_usage(&self, usage: &TurnUsage, ctx: &mut AppContext) {
        use api::response_event::stream_finished;

        // Claude-sized default when the CLI doesn't report a window.
        const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

        let context_tokens = usage.input_tokens
            + usage.cache_read_input_tokens
            + usage.cache_creation_input_tokens
            + usage.output_tokens;
        if context_tokens == 0 {
            return;
        }
        let context_window = usage
            .context_window
            .filter(|window| *window > 0)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let context_window_usage =
            (context_tokens as f64 / context_window as f64).clamp(0.0, 1.0) as f32;

        let model_id = usage
            .model_id
            .clone()
            .unwrap_or_else(|| "claude".to_string());
        let as_u32 = |tokens: u64| tokens.min(u64::from(u32::MAX)) as u32;
        let turn_tokens = stream_finished::TokenUsage {
            model_id: model_id.clone(),
            total_input: as_u32(
                usage.input_tokens
                    + usage.cache_read_input_tokens
                    + usage.cache_creation_input_tokens,
            ),
            output: as_u32(usage.output_tokens),
            input_cache_read: as_u32(usage.cache_read_input_tokens),
            input_cache_write: as_u32(usage.cache_creation_input_tokens),
            cost_in_cents: usage
                .cost_usd
                .map(|cost| (cost * 100.0) as f32)
                .unwrap_or(0.0),
        };

        let history = BlocklistAIHistoryModel::handle(ctx);
        history.update(ctx, |history, ctx| {
            // Accumulate this run into the conversation's running per-model
            // totals...
            history.update_conversation_cost_and_usage_for_request(
                self.conversation_id,
                None,
                vec![turn_tokens],
                None,
                true,
                ctx,
            );

            // ...then project those totals into the usage metadata the
            // footer reads (per-model token rows + the context meter).
            let byok_token_usage: HashMap<String, stream_finished::ModelTokenUsage> = history
                .conversation(&self.conversation_id)
                .map(|conversation| {
                    conversation
                        .total_token_usage()
                        .into_iter()
                        .map(|totals| {
                            let total_tokens = totals.total_input.saturating_add(totals.output);
                            (
                                totals.model_id.clone(),
                                stream_finished::ModelTokenUsage {
                                    model_id: totals.model_id,
                                    total_tokens,
                                    token_usage_by_category: HashMap::from([(
                                        "primary_agent".to_string(),
                                        total_tokens,
                                    )]),
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            let metadata = stream_finished::ConversationUsageMetadata {
                context_window_usage,
                credits_spent: 0.0,
                platform_credits_spent: 0.0,
                summarized: false,
                token_usage: vec![],
                tool_usage_metadata: None,
                total_input_tokens: as_u32(context_tokens),
                krust_token_usage: HashMap::new(),
                byok_token_usage,
                context_window_segments: Vec::new(),
                custom_endpoint_token_usage: HashMap::new(),
            };
            history.update_conversation_cost_and_usage_for_request(
                self.conversation_id,
                None,
                Vec::new(),
                Some(metadata),
                true,
                ctx,
            );
        });
    }

    /// Marks the exchange finished, clearing the in-progress spinner.
    pub(crate) fn complete(&self, ctx: &mut AppContext) {
        let history = BlocklistAIHistoryModel::handle(ctx);
        history.update(ctx, |history, ctx| {
            history.mark_response_stream_completed_successfully(
                &self.stream_id,
                self.conversation_id,
                self.terminal_surface_id,
                ctx,
            );
            // An output that carries tool-call actions is left InProgress by
            // the marker above — upstream expects the action flow (execute →
            // follow-up request) to finish the conversation. Local harness
            // actions are a transcript the CLI already resolved, so the run
            // ending IS the end of the turn: flip the status here or the
            // "Thinking..." shimmer never stops.
            history.update_conversation_status(
                self.terminal_surface_id,
                self.conversation_id,
                ConversationStatus::Success,
                ctx,
            );
        });
    }

    /// Closes the current exchange and opens the next one on the same
    /// conversation, with the finished tool calls' results as its input —
    /// the cloud turn shape (an output ends at its tool calls; the results
    /// come back as the next request's input). Restore derives this same
    /// shape from the persisted messages, and it is what lets inserted
    /// command blocks interleave with the response instead of trailing it.
    /// Consumes the stream; stream subsequent events into the returned one.
    pub(crate) fn continue_after_tool_results(
        self,
        results: Vec<crate::ai::agent::AIAgentActionResult>,
        ctx: &mut AppContext,
    ) -> Self {
        log::info!(
            "fork: splitting exchange after {} tool result(s)",
            results.len()
        );
        let history = BlocklistAIHistoryModel::handle(ctx);

        // Complete the current exchange. Its output ends with tool-call
        // actions, so `mark_request_completed` leaves the conversation
        // InProgress — `complete` stamps the final Success at run end.
        history.update(ctx, |history, ctx| {
            history.mark_response_stream_completed_successfully(
                &self.stream_id,
                self.conversation_id,
                self.terminal_surface_id,
                ctx,
            );
        });

        let request_id = uuid::Uuid::new_v4().to_string();
        let stream_init = api::response_event::StreamInit {
            request_id: request_id.clone(),
            conversation_id: String::new(),
            run_id: String::new(),
        };
        let stream_id = ResponseStreamId::for_shared_session(&stream_init);
        let next = Self {
            conversation_id: self.conversation_id,
            stream_id,
            root_task_id: self.root_task_id.clone(),
            request_id,
            terminal_surface_id: self.terminal_surface_id,
            skill_path_origin: SkillPathOrigin::Local,
            streaming_text_message_id: None,
            shown_texts: Vec::new(),
            streaming_reasoning_message_id: None,
        };

        let request_input = next.request_input_with(
            results
                .into_iter()
                .map(|result| AIAgentInput::ActionResult {
                    result,
                    context: Default::default(),
                })
                .collect(),
        );
        history.update(ctx, |history, ctx| {
            if let Err(error) = history.update_conversation_for_new_request_input(
                request_input,
                next.stream_id.clone(),
                next.terminal_surface_id,
                ctx,
            ) {
                log::warn!(
                    "local_harness_stream: failed to register continuation exchange: {error}"
                );
            }

            history.initialize_output_for_response_stream(
                &next.stream_id,
                next.conversation_id,
                next.terminal_surface_id,
                stream_init,
                ctx,
            );

            // The continuation is still the same run; keep the status
            // authoritative for the shimmer.
            history.mark_conversation_in_progress(
                next.conversation_id,
                next.terminal_surface_id,
                ctx,
            );
        });

        next
    }

    fn request_input(&self, prompt: &str) -> RequestInput {
        self.request_input_with(vec![AIAgentInput::UserQuery {
            query: prompt.to_string(),
            context: Default::default(),
            static_query_type: None,
            referenced_attachments: Default::default(),
            user_query_mode: UserQueryMode::default(),
            running_command: None,
            intended_agent: None,
        }])
    }

    fn request_input_with(&self, inputs: Vec<AIAgentInput>) -> RequestInput {
        // Model ids are just metadata stamped on the exchange; the actual model
        // is whatever the `claude` CLI is configured to use.
        let model_id = LLMId::from("claude");
        RequestInput {
            conversation_id: self.conversation_id,
            input_messages: HashMap::from([(self.root_task_id.clone(), inputs)]),
            working_directory: None,
            model_id: model_id.clone(),
            coding_model_id: model_id.clone(),
            cli_agent_model_id: model_id.clone(),
            computer_use_model_id: model_id,
            shared_session_response_initiator: None,
            request_start_ts: Local::now(),
            supported_tools_override: None,
        }
    }
}

#[cfg(test)]
#[path = "local_harness_stream_tests.rs"]
mod tests;
