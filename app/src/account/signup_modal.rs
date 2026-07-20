//! Signup / login modal for the fork's account server.
//!
//! A native form (no browser, no Firebase): a server address, an email, and a
//! masked password, with "Log in" and "Create account" buttons. It drives
//! [`AccountSession`](super::AccountSession) — on success the session flips the
//! account-active flag, which unlocks the swear/misery "off" toggle.
//!
//! Structurally modelled on `auth::paste_auth_token_modal`, the codebase's other
//! single-purpose text-entry modal, so it reuses the same `EditorView` inputs,
//! `ui_builder` chrome, and dim-backdrop presentation.

use pathfinder_color::ColorU;
use ui_components::{button, Component as _, Options as _};
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Align, Border, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Dismiss, Fill,
    Flex, FormattedTextElement, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement,
    Radius, Shrinkable, Stack,
};
use warpui::fonts::Weight;
use warpui::keymap::{FixedBinding, Keystroke};
use warpui::text_layout::TextAlignment;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::{AccountEvent, AccountSession};
use crate::appearance::Appearance;
use crate::editor::{
    EditorView, InteractionState, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::themes::theme::Fill as ThemeFill;

const MODAL_WIDTH: f32 = 460.;
const INPUT_BORDER_RADIUS: Radius = Radius::Pixels(4.);

pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;
    app.register_fixed_bindings([FixedBinding::new(
        "escape",
        AccountModalAction::Cancel,
        id!(AccountModalView::ui_name()),
    )]);
}

#[derive(Clone, Copy, Debug)]
pub enum AccountModalAction {
    /// Sign in to an existing account.
    LogIn,
    /// Create a new account.
    SignUp,
    /// Close without doing anything.
    Cancel,
}

#[derive(Clone, Debug)]
pub enum AccountModalEvent {
    /// The user closed the modal (cancelled, or dismissed after signing in).
    Closed,
}

pub struct AccountModalView {
    server_input: ViewHandle<EditorView>,
    email_input: ViewHandle<EditorView>,
    password_input: ViewHandle<EditorView>,
    close_mouse_state: MouseStateHandle,
    /// Status line under the inputs: an error, or a success confirmation.
    status: Option<Status>,
    /// True while a request is in flight (inputs disabled).
    busy: bool,
}

#[derive(Clone)]
enum Status {
    Error(String),
    Success(String),
}

impl AccountModalView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let server_input = Self::make_input(ctx, "https://host:10101", false);
        let email_input = Self::make_input(ctx, "you@example.com", false);
        let password_input = Self::make_input(ctx, "password", true);

        // Prefill the server address if one was configured before.
        if let Some(url) = AccountSession::server_url(ctx) {
            server_input.update(ctx, |editor, ctx| editor.set_buffer_text(&url, ctx));
        }

        // Enter in any field attempts a log in (the common case).
        for handle in [&server_input, &email_input, &password_input] {
            ctx.subscribe_to_view(handle, |me, _, event, ctx| {
                use crate::editor::Event::{AltEnter, CmdEnter, Enter, ShiftEnter};
                if matches!(event, Enter | AltEnter | CmdEnter | ShiftEnter) {
                    me.submit(false, ctx);
                }
                ctx.notify();
            });
        }

        // React to the account session's result.
        ctx.subscribe_to_model(&AccountSession::handle(ctx), |me, _, event, ctx| {
            match event {
                AccountEvent::SignedIn(account) => {
                    me.busy = false;
                    me.status = Some(Status::Success(format!("Signed in as {}", account.email)));
                    me.set_inputs_enabled(true, ctx);
                }
                AccountEvent::Failed(message) => {
                    me.busy = false;
                    me.status = Some(Status::Error(message.clone()));
                    me.set_inputs_enabled(true, ctx);
                }
                AccountEvent::SignedOut => {}
            }
            ctx.notify();
        });

        Self {
            server_input,
            email_input,
            password_input,
            close_mouse_state: MouseStateHandle::default(),
            status: None,
            busy: false,
        }
    }

    fn make_input(
        ctx: &mut ViewContext<Self>,
        placeholder: &str,
        is_password: bool,
    ) -> ViewHandle<EditorView> {
        let placeholder = placeholder.to_owned();
        ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::as_ref(ctx);
            let theme = appearance.theme();
            let bg_solid = theme.surface_2().into_solid();
            let default_color = ThemeFill::Solid(internal_colors::text_main(theme, bg_solid));
            let disabled_color = ThemeFill::Solid(internal_colors::text_disabled(theme, bg_solid));
            let hint_color = ThemeFill::Solid(internal_colors::text_sub(theme, bg_solid));
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(12.),
                        font_family_override: Some(appearance.ui_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color,
                            disabled_color,
                            hint_color,
                        }),
                        ..Default::default()
                    },
                    soft_wrap: false,
                    is_password,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text(&placeholder, ctx);
            editor
        })
    }

    fn set_inputs_enabled(&mut self, enabled: bool, ctx: &mut ViewContext<Self>) {
        let state = if enabled {
            InteractionState::Editable
        } else {
            InteractionState::Disabled
        };
        for handle in [&self.server_input, &self.email_input, &self.password_input] {
            handle.update(ctx, |editor, ctx| editor.set_interaction_state(state, ctx));
        }
    }

    /// Reads the fields and kicks off a login (`signup == false`) or signup.
    fn submit(&mut self, signup: bool, ctx: &mut ViewContext<Self>) {
        if self.busy {
            return;
        }
        let server = self.server_input.as_ref(ctx).buffer_text(ctx).trim().to_owned();
        let email = self.email_input.as_ref(ctx).buffer_text(ctx).trim().to_owned();
        let password = self.password_input.as_ref(ctx).buffer_text(ctx);

        if server.is_empty() {
            self.status = Some(Status::Error("Enter the account server address.".to_owned()));
            ctx.notify();
            return;
        }
        if email.is_empty() || password.is_empty() {
            self.status = Some(Status::Error("Enter an email and password.".to_owned()));
            ctx.notify();
            return;
        }

        // Record the server address (trust-on-first-use pins the certificate on
        // the first connect) before authenticating.
        AccountSession::set_server(ctx, &server, None);

        self.status = None;
        self.busy = true;
        self.set_inputs_enabled(false, ctx);
        AccountSession::handle(ctx).update(ctx, |session, ctx| {
            if signup {
                session.sign_up(email, password, ctx);
            } else {
                session.log_in(email, password, ctx);
            }
        });
        ctx.notify();
    }
}

impl Entity for AccountModalView {
    type Event = AccountModalEvent;
}

impl View for AccountModalView {
    fn ui_name() -> &'static str {
        "AccountModalView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.email_input);
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

        let title = FormattedTextElement::from_str(
            "Account",
            appearance.ui_font_family(),
            16.,
        )
        .with_color(internal_colors::text_main(theme, dialog_surface_solid))
        .with_weight(Weight::Bold)
        .with_line_height_ratio(1.25)
        .finish();

        let close_button = ui_builder
            .close_button(24., self.close_mouse_state.clone())
            .build()
            .on_click(|ctx: &mut warpui::EventContext, _, _| {
                ctx.dispatch_typed_action(AccountModalAction::Cancel);
            })
            .finish();

        let title_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(Shrinkable::new(1., title).finish())
            .with_child(close_button)
            .finish();

        let subtitle = FormattedTextElement::from_str(
            "Sign in to sync settings and turn swear mode on and off. \
             No account needed for anything else.",
            appearance.ui_font_family(),
            13.,
        )
        .with_color(internal_colors::text_sub(theme, dialog_surface_solid))
        .with_alignment(TextAlignment::Left)
        .with_line_height_ratio(1.3)
        .finish();

        let input_style = UiComponentStyles {
            background: Some(input_bg.into()),
            border_width: Some(1.),
            border_color: Some(Fill::Solid(border_color)),
            border_radius: Some(CornerRadius::with_all(INPUT_BORDER_RADIUS)),
            font_color: Some(input_text_color),
            padding: Some(Coords {
                top: 10.,
                bottom: 10.,
                left: 12.,
                right: 12.,
            }),
            ..Default::default()
        };

        // A field label (owned; `from_str` copies the &'static str into the element).
        let make_label = |text: &'static str| {
            FormattedTextElement::from_str(text, appearance.ui_font_family(), 12.)
                .with_color(internal_colors::text_sub(theme, dialog_surface_solid))
                .finish()
        };

        // Build the inputs at render scope — `text_input` borrows `ui_builder`,
        // so it cannot be built inside a closure that returns the element.
        let server_input_el = ui_builder
            .text_input(self.server_input.clone())
            .with_style(input_style.clone())
            .build()
            .finish();
        let email_input_el = ui_builder
            .text_input(self.email_input.clone())
            .with_style(input_style.clone())
            .build()
            .finish();
        let password_input_el = ui_builder
            .text_input(self.password_input.clone())
            .with_style(input_style.clone())
            .build()
            .finish();

        let field = |label, input| {
            Container::new(
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(label)
                    .with_child(Container::new(input).with_margin_top(4.).finish())
                    .finish(),
            )
            .with_margin_top(12.)
            .finish()
        };

        let mut body = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(subtitle)
                    .with_margin_top(8.)
                    .with_margin_bottom(4.)
                    .finish(),
            )
            .with_child(field(make_label("Server"), server_input_el))
            .with_child(field(make_label("Email"), email_input_el))
            .with_child(field(make_label("Password"), password_input_el));

        if let Some(status) = &self.status {
            let (text, color) = match status {
                Status::Error(message) => (message.clone(), theme.ui_error_color()),
                Status::Success(message) => (
                    message.clone(),
                    internal_colors::text_main(theme, dialog_surface_solid),
                ),
            };
            let status_text =
                FormattedTextElement::from_str(text, appearance.ui_font_family(), 13.)
                    .with_color(color)
                    .with_line_height_ratio(1.25)
                    .finish();
            body = body.with_child(Container::new(status_text).with_margin_top(12.).finish());
        }

        let body = body.finish();

        let log_in_button = button::Button::default().render(
            appearance,
            button::Params {
                content: button::Content::Label("Log in".into()),
                theme: &button::themes::Primary,
                options: button::Options {
                    keystroke: Keystroke::parse("enter").ok(),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(AccountModalAction::LogIn);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let sign_up_button = button::Button::default().render(
            appearance,
            button::Params {
                content: button::Content::Label("Create account".into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(AccountModalAction::SignUp);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let footer = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::End)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(sign_up_button)
                .with_child(Container::new(log_in_button).with_margin_left(8.).finish())
                .finish(),
        )
        .with_border(Border::top(1.).with_border_color(border_color))
        .with_horizontal_padding(24.)
        .with_vertical_padding(12.)
        .finish();

        let dialog = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(title_row)
                    .with_horizontal_padding(24.)
                    .with_padding_top(24.)
                    .with_padding_bottom(4.)
                    .finish(),
            )
            .with_child(
                Container::new(body)
                    .with_horizontal_padding(24.)
                    .with_padding_bottom(16.)
                    .finish(),
            )
            .with_child(footer)
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
            Container::new(warpui::elements::Empty::new().finish())
                .with_background_color(ColorU::new(0, 0, 0, 179))
                .finish(),
        );
        stack.add_child(
            Dismiss::new(Align::new(modal).finish())
                .on_dismiss(|ctx, _app| {
                    ctx.dispatch_typed_action(AccountModalAction::Cancel);
                })
                .finish(),
        );
        stack.finish()
    }
}

impl TypedActionView for AccountModalView {
    type Action = AccountModalAction;

    fn handle_action(&mut self, action: &AccountModalAction, ctx: &mut ViewContext<Self>) {
        match action {
            AccountModalAction::LogIn => self.submit(false, ctx),
            AccountModalAction::SignUp => self.submit(true, ctx),
            AccountModalAction::Cancel => ctx.emit(AccountModalEvent::Closed),
        }
    }
}
