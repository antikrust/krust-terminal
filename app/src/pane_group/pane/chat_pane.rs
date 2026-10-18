//! Fork: a dedicated global-chat pane, opened as a tab.
//!
//! Hosts the same chat room as the command-palette "Chat" modal
//! ([`crate::account::chat`]) — the shared [`ChatModel`] polls the account
//! server's `/v1/chat` — but as a first-class split pane so it lives in the
//! tabs menu alongside Settings and terminals. Modeled on
//! [`super::mcp_servers_pane`]: at most one pane per window (tracked by
//! [`ChatPaneManager`]), not persisted across restarts (`LeafContents::Chat` is
//! non-persisted; reopening from the palette is one action).

use std::collections::HashMap;

use pathfinder_color::ColorU;
use ui_components::{button, Component as _, Options as _};
use krust_core::ui::theme::color::internal_colors;
use krustui::elements::{
    Border, Container, CrossAxisAlignment, Fill, Flex, FormattedTextElement, MainAxisSize,
    ParentElement, Shrinkable,
};
use krustui::keymap::Keystroke;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::{
    AppContext, Element, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use super::view::PaneView;
use super::{
    view, DetachType, PaneConfiguration, PaneContent, PaneEvent, PaneGroup, PaneId, ShareableLink,
    ShareableLinkError,
};
use crate::account::chat::ChatModel;
use crate::app_state::LeafContents;
use crate::appearance::Appearance;
use crate::editor::{EditorView, SingleLineEditorOptions, TextColors, TextOptions};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::BackingView;
use crate::themes::theme::Fill as ThemeFill;
use crate::workspace::PaneViewLocator;

const HEADER_TEXT: &str = "Chat";
const PANE_PADDING: f32 = 16.;
/// How many recent messages to show (no scrollback yet).
const VISIBLE_MESSAGES: usize = 200;

#[derive(Debug, Clone)]
pub enum ChatPaneViewEvent {
    Pane(PaneEvent),
}

#[derive(Clone, Copy, Debug)]
pub enum ChatPaneViewAction {
    Send,
}

/// The pane's backing view: the chat room content (a recent-message list and an
/// input row), driven by the shared [`ChatModel`].
pub struct ChatPaneView {
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
    input: ViewHandle<EditorView>,
}

impl ChatPaneView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_ctx| PaneConfiguration::new(HEADER_TEXT));

        let input = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let theme = appearance.theme();
            let bg_solid = theme.surface_2().into_solid();
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(13.),
                        font_family_override: Some(appearance.ui_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color: ThemeFill::Solid(internal_colors::text_main(
                                theme, bg_solid,
                            )),
                            disabled_color: ThemeFill::Solid(internal_colors::text_disabled(
                                theme, bg_solid,
                            )),
                            hint_color: ThemeFill::Solid(internal_colors::text_sub(
                                theme, bg_solid,
                            )),
                        }),
                        ..Default::default()
                    },
                    soft_wrap: false,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Message everyone…", ctx);
            editor
        });

        // Send on Enter from the input.
        ctx.subscribe_to_view(&input, |me, _, event, ctx| {
            use crate::editor::Event::{AltEnter, CmdEnter, Enter, ShiftEnter};
            if matches!(event, Enter | AltEnter | CmdEnter | ShiftEnter) {
                me.send(ctx);
            }
            ctx.notify();
        });

        // Repaint on new messages and keep the room polling while open.
        ctx.subscribe_to_model(&ChatModel::handle(ctx), |_me, _, _event, ctx| ctx.notify());
        ChatModel::handle(ctx).update(ctx, |model, ctx| model.start(ctx));

        Self {
            pane_configuration,
            focus_handle: None,
            input,
        }
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.input);
    }

    fn send(&mut self, ctx: &mut ViewContext<Self>) {
        let body = self.input.as_ref(ctx).buffer_text(ctx).trim().to_owned();
        if body.is_empty() {
            return;
        }
        self.input
            .update(ctx, |editor, ctx| editor.set_buffer_text("", ctx));
        ChatModel::handle(ctx).update(ctx, |model, ctx| model.send(body, ctx));
    }
}

impl Entity for ChatPaneView {
    type Event = ChatPaneViewEvent;
}

impl View for ChatPaneView {
    fn ui_name() -> &'static str {
        "ChatPaneView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            self.focus(ctx);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let surface = theme.surface_1();
        let surface_solid = surface.into_solid();
        let border_color = internal_colors::neutral_4(theme);
        let input_bg = theme.surface_2();
        let input_text_color: ColorU = internal_colors::text_main(theme, input_bg.into_solid());
        let ui_builder = appearance.ui_builder();

        // Clone the visible slice so we don't hold a model borrow across the build.
        let visible: Vec<account_client::Message> = {
            let all = ChatModel::as_ref(app).messages();
            let start = all.len().saturating_sub(VISIBLE_MESSAGES);
            all[start..].to_vec()
        };
        let mut list = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        if visible.is_empty() {
            list = list.with_child(
                FormattedTextElement::from_str(
                    "No messages yet. Say hi!",
                    appearance.ui_font_family(),
                    13.,
                )
                .with_color(internal_colors::text_sub(theme, surface_solid))
                .finish(),
            );
        }
        for message in &visible {
            let author = message
                .author_email
                .split('@')
                .next()
                .unwrap_or(&message.author_email)
                .to_owned();
            let line = FormattedTextElement::from_str(
                format!("{author}: {}", message.body),
                appearance.ui_font_family(),
                13.,
            )
            .with_color(internal_colors::text_main(theme, surface_solid))
            .with_line_height_ratio(1.3)
            .finish();
            list = list.with_child(Container::new(line).with_margin_top(4.).finish());
        }

        let input = ui_builder
            .text_input(self.input.clone())
            .with_style(UiComponentStyles {
                background: Some(input_bg.into()),
                border_width: Some(1.),
                border_color: Some(Fill::Solid(border_color)),
                font_color: Some(input_text_color),
                padding: Some(Coords {
                    top: 10.,
                    bottom: 10.,
                    left: 12.,
                    right: 12.,
                }),
                ..Default::default()
            })
            .build()
            .finish();

        let send_button = button::Button::default().render(
            appearance,
            button::Params {
                content: button::Content::Label("Send".into()),
                theme: &button::themes::Primary,
                options: button::Options {
                    keystroke: Keystroke::parse("enter").ok(),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(ChatPaneViewAction::Send);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let input_row = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1., input).finish())
                .with_child(Container::new(send_button).with_margin_left(8.).finish())
                .finish(),
        )
        .with_border(Border::top(1.).with_border_color(border_color))
        .with_margin_top(12.)
        .with_padding_top(12.)
        .finish();

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(Shrinkable::new(1., list.finish()).finish())
                .with_child(input_row)
                .finish(),
        )
        .with_uniform_padding(PANE_PADDING)
        .finish()
    }
}

impl TypedActionView for ChatPaneView {
    type Action = ChatPaneViewAction;

    fn handle_action(&mut self, action: &ChatPaneViewAction, ctx: &mut ViewContext<Self>) {
        match action {
            ChatPaneViewAction::Send => self.send(ctx),
        }
    }
}

impl BackingView for ChatPaneView {
    type PaneHeaderOverflowMenuAction = ();
    type CustomAction = ();
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        _action: &Self::PaneHeaderOverflowMenuAction,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(ChatPaneViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        _app: &AppContext,
    ) -> view::HeaderContent {
        view::HeaderContent::simple(HEADER_TEXT)
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}

pub struct ChatPane {
    view: ViewHandle<PaneView<ChatPaneView>>,
    pane_configuration: ModelHandle<PaneConfiguration>,
}

impl ChatPane {
    pub fn from_view(chat_pane_view: ViewHandle<ChatPaneView>, ctx: &mut AppContext) -> Self {
        let pane_configuration = chat_pane_view.as_ref(ctx).pane_configuration();

        let view = ctx.add_typed_action_view(chat_pane_view.window_id(ctx), |ctx| {
            let pane_id = PaneId::from_chat_pane_ctx(ctx);
            PaneView::new(pane_id, chat_pane_view, (), pane_configuration.clone(), ctx)
        });

        Self {
            view,
            pane_configuration,
        }
    }

    pub fn new<V: View>(ctx: &mut ViewContext<V>) -> Self {
        let view = ctx.add_typed_action_view(ChatPaneView::new);
        Self::from_view(view, ctx)
    }

    pub fn chat_pane_view(&self, ctx: &AppContext) -> ViewHandle<ChatPaneView> {
        self.view.as_ref(ctx).child(ctx)
    }
}

impl PaneContent for ChatPane {
    fn id(&self) -> PaneId {
        PaneId::from_chat_pane_view(&self.view)
    }

    fn snapshot(&self, _app: &AppContext) -> LeafContents {
        LeafContents::Chat
    }

    fn attach(
        &self,
        _group: &PaneGroup,
        focus_handle: PaneFocusHandle,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        self.view
            .update(ctx, |view, ctx| view.set_focus_handle(focus_handle, ctx));

        let chat_pane_view = self.chat_pane_view(ctx);
        let pane_id = self.id();
        let pane_group_id = ctx.view_id();
        let window_id = ctx.window_id();

        ctx.subscribe_to_view(&chat_pane_view, move |pane_group, _, event, ctx| {
            let ChatPaneViewEvent::Pane(pane_event) = event;
            pane_group.handle_pane_event(pane_id, pane_event, ctx)
        });
        ctx.subscribe_to_view(&self.view, move |group, _, event, ctx| {
            group.handle_pane_view_event(pane_id, event, ctx);
        });

        ChatPaneManager::handle(ctx).update(ctx, |manager, _ctx| {
            manager.register_pane(
                window_id,
                PaneViewLocator {
                    pane_group_id,
                    pane_id,
                },
            );
        });
    }

    fn detach(
        &self,
        _group: &PaneGroup,
        _detach_type: DetachType,
        ctx: &mut ViewContext<PaneGroup>,
    ) {
        let chat_pane_view = self.chat_pane_view(ctx);
        ctx.unsubscribe_to_view(&chat_pane_view);
        ctx.unsubscribe_to_view(&self.view);

        let window_id = ctx.window_id();
        ChatPaneManager::handle(ctx).update(ctx, |manager, _| {
            manager.deregister_pane(&window_id);
        });
    }

    fn has_application_focus(&self, ctx: &mut ViewContext<PaneGroup>) -> bool {
        self.view.is_self_or_child_focused(ctx)
    }

    fn focus(&self, ctx: &mut ViewContext<PaneGroup>) {
        self.chat_pane_view(ctx)
            .update(ctx, |view, ctx| view.focus(ctx));
    }

    fn shareable_link(
        &self,
        _ctx: &mut ViewContext<PaneGroup>,
    ) -> Result<ShareableLink, ShareableLinkError> {
        Ok(ShareableLink::Base)
    }

    fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    fn is_pane_being_dragged(&self, ctx: &AppContext) -> bool {
        self.view.as_ref(ctx).is_being_dragged()
    }
}

/// Singleton mapping `WindowId -> PaneViewLocator` for open chat panes, so each
/// window shows at most one and reopening focuses the existing pane.
#[derive(Default)]
pub struct ChatPaneManager {
    panes: HashMap<WindowId, PaneViewLocator>,
}

impl ChatPaneManager {
    pub fn find_pane(&self, window_id: WindowId) -> Option<PaneViewLocator> {
        self.panes.get(&window_id).copied()
    }

    pub fn register_pane(&mut self, window_id: WindowId, locator: PaneViewLocator) {
        self.panes.insert(window_id, locator);
    }

    pub fn deregister_pane(&mut self, window_id: &WindowId) {
        self.panes.remove(window_id);
    }
}

impl Entity for ChatPaneManager {
    type Event = ();
}

impl SingletonEntity for ChatPaneManager {}
