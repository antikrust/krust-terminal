//! Global chat: a model that polls the account server's `/v1/chat` room, and a
//! modal to read and post to it.
//!
//! One global room for now (every signed-in account sees everything); teams
//! come later via the server's `messages.team` column. The transport is
//! `account_client`'s tested `get_chat` / `send_chat`.
//!
//! This is delivered as a command-palette modal ("Chat") rather than a
//! first-class tab: a tab means a full `PaneContent`/`IPaneType` integration
//! across the pane system, which is a large change best done with the app
//! runnable. The modal reuses the same view scaffolding as the signup modal and
//! gets a working chat window now; promoting it to a tab is a follow-up that
//! wraps this same view in a `ChatPane`.

use std::time::Duration;

use account_client::{AccountError, Message};
use pathfinder_color::ColorU;
use ui_components::{button, Component as _, Options as _};
use krust_core::ui::theme::color::internal_colors;
use krustui::elements::{
    Align, Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss, Fill,
    Flex, FormattedTextElement, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement,
    Radius, Shrinkable, Stack,
};
use krustui::fonts::Weight;
use krustui::keymap::{FixedBinding, Keystroke};
use krustui::r#async::Timer;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::{
    AppContext, Element, Entity, FocusContext, ModelContext, SingletonEntity, TypedActionView,
    View, ViewContext, ViewHandle,
};

use super::AccountSession;
use crate::appearance::Appearance;
use crate::editor::{EditorView, SingleLineEditorOptions, TextColors, TextOptions};
use crate::themes::theme::Fill as ThemeFill;

const MODAL_WIDTH: f32 = 520.;
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// How many recent messages the modal shows (no scrollback yet).
const VISIBLE_MESSAGES: usize = 40;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

pub enum ChatEvent {
    /// The message list changed.
    Updated,
}

/// Singleton that owns the chat room's messages and polls the server for new
/// ones while a reader is active.
pub struct ChatModel {
    messages: Vec<Message>,
    /// Highest message id we've seen; the next poll asks for ids above it.
    last_id: i32,
    /// Whether a reader (the modal) is currently open. Polling reschedules only
    /// while true, so closing the modal lets the loop wind down on its own.
    active: bool,
}

impl ChatModel {
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            messages: Vec::new(),
            last_id: 0,
            active: false,
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Begins polling. Idempotent: a second call while already active is a no-op
    /// so opening the modal twice doesn't spawn two loops.
    pub fn start(&mut self, ctx: &mut ModelContext<Self>) {
        if self.active {
            ctx.emit(ChatEvent::Updated);
            return;
        }
        self.active = true;
        self.poll_now(ctx);
    }

    /// Stops rescheduling. An in-flight timer fires once more and no-ops.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Posts a message; the following poll (kicked immediately) picks it up.
    pub fn send(&self, body: String, ctx: &mut ModelContext<Self>) {
        let Some(token) = AccountSession::as_ref(ctx).token() else {
            return;
        };
        let Ok(client) = AccountSession::client(ctx) else {
            return;
        };
        let _ = ctx.spawn(
            async move {
                let _ = client.send_chat(&token, &body).await;
            },
            |me, _res: (), ctx| me.poll_now(ctx),
        );
    }

    fn poll_now(&self, ctx: &mut ModelContext<Self>) {
        if !self.active {
            return;
        }
        let Some(token) = AccountSession::as_ref(ctx).token() else {
            return;
        };
        let Ok(client) = AccountSession::client(ctx) else {
            return;
        };
        let since = if self.last_id > 0 {
            Some(self.last_id)
        } else {
            None
        };
        let _ = ctx.spawn(
            async move { client.get_chat(&token, since).await },
            Self::on_fetched,
        );
    }

    fn on_fetched(
        &mut self,
        result: Result<Vec<Message>, AccountError>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Ok(fetched) = result {
            for message in fetched {
                // Guard on last_id so overlapping polls never double-insert.
                if message.id > self.last_id {
                    self.last_id = message.id;
                    self.messages.push(message);
                }
            }
            ctx.emit(ChatEvent::Updated);
        }
        if self.active {
            let _ = ctx.spawn(
                async move { Timer::after(POLL_INTERVAL).await },
                |me, _, ctx| me.poll_now(ctx),
            );
        }
    }
}

impl Entity for ChatModel {
    type Event = ChatEvent;
}

impl SingletonEntity for ChatModel {}

// ---------------------------------------------------------------------------
// Modal
// ---------------------------------------------------------------------------

pub fn init(app: &mut AppContext) {
    use krustui::keymap::macros::*;
    app.register_fixed_bindings([
        FixedBinding::new(
            "escape",
            ChatModalAction::Cancel,
            id!(ChatModalView::ui_name()),
        ),
        FixedBinding::new(
            "enter",
            ChatModalAction::Send,
            id!(ChatModalView::ui_name()),
        ),
    ]);
}

#[derive(Clone, Copy, Debug)]
pub enum ChatModalAction {
    Send,
    Cancel,
}

#[derive(Clone, Debug)]
pub enum ChatModalEvent {
    Closed,
}

pub struct ChatModalView {
    input: ViewHandle<EditorView>,
    close_mouse_state: MouseStateHandle,
}

impl ChatModalView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
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

        // Repaint when the room changes, and start polling while open.
        ctx.subscribe_to_model(&ChatModel::handle(ctx), |_me, _, _event, ctx| {
            ctx.notify();
        });
        ChatModel::handle(ctx).update(ctx, |model, ctx| model.start(ctx));

        Self {
            input,
            close_mouse_state: MouseStateHandle::default(),
        }
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

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ChatModel::handle(ctx).update(ctx, |model, _| model.stop());
        ctx.emit(ChatModalEvent::Closed);
    }
}

impl Entity for ChatModalView {
    type Event = ChatModalEvent;
}

impl View for ChatModalView {
    fn ui_name() -> &'static str {
        "ChatModalView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.input);
            ctx.notify();
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let dialog_surface = theme.surface_1();
        let dialog_surface_solid = dialog_surface.into_solid();
        let border_color = internal_colors::neutral_4(theme);
        let input_bg = theme.surface_2();
        let input_bg_solid = input_bg.into_solid();
        let input_text_color: ColorU = internal_colors::text_main(theme, input_bg_solid);
        let ui_builder = appearance.ui_builder();

        let title = FormattedTextElement::from_str("Chat", appearance.ui_font_family(), 16.)
            .with_color(internal_colors::text_main(theme, dialog_surface_solid))
            .with_weight(Weight::Bold)
            .finish();

        let close_button = ui_builder
            .close_button(24., self.close_mouse_state.clone())
            .build()
            .on_click(|ctx: &mut krustui::EventContext, _, _| {
                ctx.dispatch_typed_action(ChatModalAction::Cancel);
            })
            .finish();

        let title_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(Shrinkable::new(1., title).finish())
            .with_child(close_button)
            .finish();

        // Message list: the most recent messages, oldest at the top. Clone the
        // visible slice so we don't hold a borrow of the model across the build.
        let visible: Vec<Message> = {
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
                .with_color(internal_colors::text_sub(theme, dialog_surface_solid))
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
            .with_color(internal_colors::text_main(theme, dialog_surface_solid))
            .with_line_height_ratio(1.3)
            .finish();
            list = list.with_child(Container::new(line).with_margin_top(4.).finish());
        }
        let list = ConstrainedBox::new(list.finish())
            .with_height(320.)
            .finish();

        let input = ui_builder
            .text_input(self.input.clone())
            .with_style(UiComponentStyles {
                background: Some(input_bg.into()),
                border_width: Some(1.),
                border_color: Some(Fill::Solid(border_color)),
                border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
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
                        ctx.dispatch_typed_action(ChatModalAction::Send);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let input_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Shrinkable::new(1., input).finish())
            .with_child(Container::new(send_button).with_margin_left(8.).finish())
            .finish();

        let dialog = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(title_row)
                    .with_horizontal_padding(20.)
                    .with_padding_top(20.)
                    .with_padding_bottom(12.)
                    .finish(),
            )
            .with_child(
                Container::new(list)
                    .with_horizontal_padding(20.)
                    .with_padding_bottom(12.)
                    .finish(),
            )
            .with_child(
                Container::new(input_row)
                    .with_border(Border::top(1.).with_border_color(border_color))
                    .with_horizontal_padding(20.)
                    .with_vertical_padding(12.)
                    .finish(),
            )
            .finish();

        let modal = ConstrainedBox::new(
            Container::new(dialog)
                .with_background(dialog_surface)
                .with_border(Border::all(1.).with_border_color(border_color))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                .finish(),
        )
        .with_width(MODAL_WIDTH)
        .finish();

        let mut stack = Stack::new();
        stack.add_child(
            Container::new(krustui::elements::Empty::new().finish())
                .with_background_color(ColorU::new(0, 0, 0, 179))
                .finish(),
        );
        stack.add_child(
            Dismiss::new(Align::new(modal).finish())
                .on_dismiss(|ctx, _app| {
                    ctx.dispatch_typed_action(ChatModalAction::Cancel);
                })
                .finish(),
        );
        stack.finish()
    }
}

impl TypedActionView for ChatModalView {
    type Action = ChatModalAction;

    fn handle_action(&mut self, action: &ChatModalAction, ctx: &mut ViewContext<Self>) {
        match action {
            ChatModalAction::Send => self.send(ctx),
            ChatModalAction::Cancel => self.close(ctx),
        }
    }
}
