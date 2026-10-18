use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use itertools::Itertools;
use lazy_static::lazy_static;
use settings::Setting as _;
use krust_completer::completer::TopLevelCommandCaseSensitivity;
use krust_completer::parsers::classify_command;
use krust_completer::parsers::hir::{Command, Expression};
use krust_completer::parsers::simple::all_parsed_commands;
use krust_completer::signatures::CommandRegistry;
use krust_errors::report_if_error;
use krust_util::path::EscapeChar;
use krustui::accessibility::{AccessibilityContent, ActionAccessibilityContent, KrustA11yRole};
use krustui::{SingletonEntity, ViewContext};

use super::{Event, InlineBannerItem, InlineBannerType, TerminalView};
#[cfg(feature = "local_fs")]
use crate::code::editor_management::CodeSource;
use crate::terminal::event::UserBlockCompleted;
use crate::terminal::general_settings::GeneralSettings;
use crate::terminal::model::session::Session;
use crate::terminal::view::inline_banner::{OpenInKrustBannerAction, OpenInKrustBannerState};
use crate::util::openable_file_type::{
    is_file_openable_in_krust, renders_in_krust_notebook_viewer, OpenableFileType,
};

#[cfg(test)]
#[path = "open_in_krust_tests.rs"]
mod tests;

const LEARN_MORE_MARKDOWN_URL: &str =
    "https://therob.lol/krust-terminal/help/terminal/more-features/markdown-viewer";
const LEARN_MORE_CODE_URL: &str =
    "https://therob.lol/krust-terminal/help/code/overview#built-in-code-editor";

/// A path to a file that can be opened in Krust, along with its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenablePath {
    pub path: PathBuf,
    pub file_type: OpenableFileType,
}

impl TerminalView {
    pub(super) fn maybe_suggest_open_in_krust(
        &mut self,
        block_completed: &UserBlockCompleted,
        ctx: &mut ViewContext<TerminalView>,
    ) {
        if let Some(active_block_metadata) = self.active_block_metadata.as_ref() {
            let Some(session) = active_block_metadata
                .session_id()
                .and_then(|id| self.sessions.as_ref(ctx).get(id))
            else {
                return;
            };
            if !session.is_local() {
                return;
            }

            let command = block_completed.command.clone();
            let working_directory = active_block_metadata
                .current_working_directory()
                .map(Into::into);
            let command_case_sensitivity = session.command_case_sensitivity();
            let escape_char = session.shell_family().escape_char();
            ctx.spawn(
                async move {
                    check_openable_in_krust(
                        command,
                        working_directory,
                        command_case_sensitivity,
                        escape_char,
                    )
                    .await
                },
                move |view, maybe_match, ctx| {
                    if let Some(openable_path) = maybe_match {
                        if renders_in_krust_notebook_viewer(&openable_path.path)
                            || matches!(openable_path.file_type, OpenableFileType::Code)
                        {
                            view.suggest_open_in_krust(openable_path, session, ctx);
                        }
                    }
                },
            );
        }
    }

    /// Whether or not the "Open in Krust" banner is open.
    #[cfg(feature = "integration_tests")]
    pub fn is_open_in_krust_banner_open(&self) -> bool {
        self.inline_banners_state.open_in_krust_banner.is_some()
    }

    fn close_open_in_krust_banner(&mut self, banner_id: usize) {
        self.model
            .lock()
            .block_list_mut()
            .remove_inline_banner(banner_id);
    }

    fn open_in_krust_banner_type_dismissed(
        &self,
        openable_path: &OpenablePath,
        ctx: &ViewContext<Self>,
    ) -> bool {
        let general_settings = GeneralSettings::as_ref(ctx);
        if renders_in_krust_notebook_viewer(&openable_path.path) {
            *general_settings.open_in_krust_banner_dismissed_for_markdown
        } else {
            *general_settings.open_in_krust_banner_dismissed_for_code_and_text
        }
    }

    /// Insert a suggestion banner for opening the file `openable_path`, originating from
    /// `session`, in a Krust pane.
    fn suggest_open_in_krust(
        &mut self,
        openable_path: OpenablePath,
        session: Arc<Session>,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.open_in_krust_banner_type_dismissed(&openable_path, ctx) {
            return;
        }

        // We only show a banner for the most recent command.
        if let Some(prev_state) = &self.inline_banners_state.open_in_krust_banner {
            self.close_open_in_krust_banner(prev_state.id);
        }

        let banner_id = self.inline_banners_state.next_banner_id();
        self.inline_banners_state.open_in_krust_banner = Some(OpenInKrustBannerState::new(
            banner_id,
            openable_path,
            session,
        ));
        self.model
            .lock()
            .block_list_mut()
            .append_inline_banner(InlineBannerItem::new(
                banner_id,
                InlineBannerType::OpenInKrust,
            ));
        ctx.notify();
    }

    pub fn handle_open_in_krust_banner_action(
        &mut self,
        action: OpenInKrustBannerAction,
        ctx: &mut ViewContext<Self>,
    ) {
        match action {
            OpenInKrustBannerAction::OpenFile => {
                if let Some(banner_state) = self.inline_banners_state.open_in_krust_banner.take() {
                    if renders_in_krust_notebook_viewer(&banner_state.target.path) {
                        // Markdown and Jupyter notebooks open in the notebook viewer.
                        ctx.emit(Event::OpenFileInKrust {
                            path: banner_state.target.path,
                            session: banner_state.session,
                        });
                    } else {
                        // Code and other text files open in the code editor.
                        #[cfg(feature = "local_fs")]
                        ctx.emit(Event::OpenCodeInKrust {
                            source: CodeSource::Link {
                                path: banner_state.target.path,
                                range_start: None,
                                range_end: None,
                            },
                            layout: *crate::terminal::view::EditorSettings::as_ref(ctx)
                                .open_file_layout
                                .value(),
                        });
                    }
                    self.close_open_in_krust_banner(banner_state.id);
                    ctx.notify();
                }
            }
            OpenInKrustBannerAction::LearnMore => {
                if let Some(banner_state) = &self.inline_banners_state.open_in_krust_banner {
                    let url = if renders_in_krust_notebook_viewer(&banner_state.target.path) {
                        LEARN_MORE_MARKDOWN_URL
                    } else {
                        LEARN_MORE_CODE_URL
                    };
                    ctx.open_url(url);
                }
            }
            OpenInKrustBannerAction::Close => {
                if let Some(banner_state) = self.inline_banners_state.open_in_krust_banner.take() {
                    self.close_open_in_krust_banner(banner_state.id);
                    if renders_in_krust_notebook_viewer(&banner_state.target.path) {
                        GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                            report_if_error!(settings
                                .open_in_krust_banner_dismissed_for_markdown
                                .set_value(true, ctx));
                        });
                    } else {
                        GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                            report_if_error!(settings
                                .open_in_krust_banner_dismissed_for_code_and_text
                                .set_value(true, ctx));
                        });
                    }
                    ctx.notify();
                }
            }
        }
    }

    pub fn open_in_krust_banner_accessibility_content(
        &self,
        action: OpenInKrustBannerAction,
    ) -> ActionAccessibilityContent {
        match action {
            OpenInKrustBannerAction::OpenFile => {
                match &self.inline_banners_state.open_in_krust_banner {
                    Some(banner_state) => {
                        ActionAccessibilityContent::Custom(AccessibilityContent::new_without_help(
                            format!("Open {} in Krust", banner_state.target.path.display()),
                            KrustA11yRole::UserAction,
                        ))
                    }
                    None => ActionAccessibilityContent::Empty,
                }
            }
            OpenInKrustBannerAction::Close => {
                ActionAccessibilityContent::Custom(AccessibilityContent::new_without_help(
                    "Close View in Krust banner",
                    KrustA11yRole::UserAction,
                ))
            }
            OpenInKrustBannerAction::LearnMore => {
                ActionAccessibilityContent::Custom(AccessibilityContent::new(
                    "Learn more",
                    "Learn more about opening Markdown files in Krust",
                    KrustA11yRole::UserAction,
                ))
            }
        }
    }
}

lazy_static! {
    static ref FILE_VIEWER_COMMANDS: HashSet<&'static str> =
        HashSet::from(["bat", "cat", "glow", "less", "open"]);
}

/// Examines `command` for a file openable in Krust, returning the resolved path and type if found.
async fn check_openable_in_krust(
    command: String,
    working_directory: Option<String>,
    command_case_sensitivity: TopLevelCommandCaseSensitivity,
    escape_char: EscapeChar,
) -> Option<OpenablePath> {
    // We can use PathBuf/Path here because, at the moment, only local sessions are supported.
    let working_directory = working_directory.map(PathBuf::from);
    for command in all_parsed_commands(command, escape_char) {
        // We want to parse the command enough to distinguish file names from arguments, but no
        // more than necessary.
        // TODO(ben): Expand aliases as well.
        let mut tokens = command.parts.iter().map(|s| s.as_str()).collect_vec();
        let command_registry = CommandRegistry::global_instance();
        let Some(classified_command) = classify_command(
            command.clone(),
            &mut tokens,
            &command_registry,
            command_case_sensitivity,
        ) else {
            continue;
        };
        if !FILE_VIEWER_COMMANDS.contains(classified_command.command.command_name_span().item) {
            continue;
        }

        // All the supported viewers take files as positional arguments.
        let positionals = match classified_command.command {
            Command::Classified(shell_command) => shell_command.args.positionals,
            Command::Unclassified(command) => command.args.positionals,
        };

        if let Some(positionals) = positionals {
            for arg in positionals.iter() {
                // Skip commands and environment variables.
                if !matches!(
                    arg.expression(),
                    Expression::Literal | Expression::ValidatableArgument(_) | Expression::Unknown
                ) {
                    continue;
                }

                let relative_path = Path::new(arg.value().as_str());

                let Some(file_type) = is_file_openable_in_krust(relative_path) else {
                    continue;
                };

                let resolved = working_directory.as_ref().map_or_else(
                    || relative_path.to_path_buf(),
                    |cwd| cwd.join(relative_path),
                );

                if async_fs::metadata(&resolved).await.is_ok() {
                    // We've found a file that exists and can be opened in Krust.
                    return Some(OpenablePath {
                        path: resolved,
                        file_type,
                    });
                }
            }
        }
    }
    None
}
