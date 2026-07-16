//! Injects a local CLI harness run (e.g. streaming Claude Code) into the agent
//! conversation UI as a live, incrementally-updating exchange — the same
//! rendering path Warp's own agent uses.
//!
//! Lives inside `blocklist` so it can reach
//! [`BlocklistAIHistoryModel::update_conversation_for_new_request_input`]
//! (which is `pub(super)`), then drives the public streaming methods
//! (`apply_client_actions`, `mark_response_stream_completed_successfully`) as
//! output arrives. Unlike the shared-session path
//! (`controller::shared_session`), this does NOT mark the conversation
//! view-only or as "viewing a shared session" — the user owns and drives it.

use std::collections::HashMap;

use chrono::Local;
use warp_multi_agent_api as api;
use warpui::{AppContext, EntityId, SingletonEntity as _};

use ai::agent::TurnUsage;
use ai::skills::SkillPathOrigin;

use super::controller::response_stream::ResponseStreamId;
use super::controller::RequestInput;
use super::history_model::BlocklistAIHistoryModel;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{AIAgentInput, UserQueryMode};
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

            history.mark_active_conversation_id(this.conversation_id, terminal_surface_id, ctx);
        });

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
                warp_token_usage: HashMap::new(),
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
        });
    }

    fn request_input(&self, prompt: &str) -> RequestInput {
        // Model ids are just metadata stamped on the exchange; the actual model
        // is whatever the `claude` CLI is configured to use.
        let model_id = LLMId::from("claude");
        RequestInput {
            conversation_id: self.conversation_id,
            input_messages: HashMap::from([(
                self.root_task_id.clone(),
                vec![AIAgentInput::UserQuery {
                    query: prompt.to_string(),
                    context: Default::default(),
                    static_query_type: None,
                    referenced_attachments: Default::default(),
                    user_query_mode: UserQueryMode::default(),
                    running_command: None,
                    intended_agent: None,
                }],
            )]),
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
