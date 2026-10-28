//! A single-line text field — Krust's own editor is far bigger than the apps' few settings fields
//! need. Click to focus; type, Backspace, Cmd-V to paste, Enter to submit. A masked field shows dots
//! (for tokens). It draws inside Krust's text-input style (`appearance.ui_builder().text_input`).

use krust_core::ui::appearance::Appearance;
use krustui::elements::{
    AfterLayoutContext, Container, DispatchEventResult, Element, Event, EventContext,
    EventHandler, LayoutContext, PaintContext, Point, SizeConstraint, Text,
};
use krustui::elements::{CornerRadius, Radius};
use krustui::event::DispatchedEvent;
use krustui::geometry::vector::Vector2F;
use krustui::{
    AppContext, BlurContext, Entity, FocusContext, SingletonEntity, TypedActionView, View,
    ViewContext,
};

pub struct TextField {
    text: String,
    placeholder: String,
    masked: bool,
    focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextFieldEvent {
    /// The text changed.
    Changed,
    /// Enter was pressed.
    Submitted,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextFieldAction {
    Focus,
    Insert(String),
    Backspace,
    Paste,
    Submit,
}

impl TextField {
    pub fn new(text: &str, placeholder: &str, masked: bool) -> Self {
        Self {
            text: text.to_owned(),
            placeholder: placeholder.to_owned(),
            masked,
            focused: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: &str, ctx: &mut ViewContext<Self>) {
        self.text = text.to_owned();
        ctx.notify();
    }

    fn insert(&mut self, chars: &str, ctx: &mut ViewContext<Self>) {
        // One line: newlines from a paste become nothing.
        self.text
            .extend(chars.chars().filter(|c| !c.is_control()));
        ctx.emit(TextFieldEvent::Changed);
        ctx.notify();
    }
}

impl Entity for TextField {
    type Event = TextFieldEvent;
}

impl View for TextField {
    fn ui_name() -> &'static str {
        "AppShellTextField"
    }

    fn on_focus(&mut self, _: &FocusContext, ctx: &mut ViewContext<Self>) {
        self.focused = true;
        ctx.notify();
    }

    fn on_blur(&mut self, _: &BlurContext, ctx: &mut ViewContext<Self>) {
        self.focused = false;
        ctx.notify();
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let (shown, color) = if self.text.is_empty() && !self.focused {
            (self.placeholder.clone(), theme.disabled_ui_text_color())
        } else if self.masked {
            ("•".repeat(self.text.chars().count()), theme.active_ui_text_color())
        } else {
            (self.text.clone(), theme.active_ui_text_color())
        };
        let shown = if self.focused { format!("{shown}▏") } else { shown };
        let text = Text::new_inline(shown, appearance.ui_font_family(), appearance.ui_font_size())
            .with_color(color.into())
            .finish();
        let field = Container::new(text)
            .with_padding_top(6.)
            .with_padding_bottom(6.)
            .with_padding_left(10.)
            .with_padding_right(10.)
            .with_background(if self.focused {
                theme.surface_3()
            } else {
                theme.surface_2()
            })
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .finish();
        EventHandler::new(
            KeyCatcher {
                child: field,
                focused: self.focused,
            }
            .finish(),
        )
        .on_left_mouse_down(|ctx, _, _| {
            ctx.dispatch_typed_action(TextFieldAction::Focus);
            DispatchEventResult::StopPropagation
        })
        .finish()
    }
}

impl TypedActionView for TextField {
    type Action = TextFieldAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            TextFieldAction::Focus => ctx.focus_self(),
            TextFieldAction::Insert(chars) => self.insert(chars, ctx),
            TextFieldAction::Backspace => {
                if self.text.pop().is_some() {
                    ctx.emit(TextFieldEvent::Changed);
                    ctx.notify();
                }
            }
            TextFieldAction::Paste => {
                let pasted = ctx.clipboard().read().plain_text;
                self.insert(&pasted, ctx);
            }
            TextFieldAction::Submit => ctx.emit(TextFieldEvent::Submitted),
        }
    }
}

/// Draws its child and, while the field is focused, turns typing into the field's actions: typed
/// characters (which come through the IME, so accents and dead keys work), Backspace, Enter, Cmd-V.
struct KeyCatcher {
    child: Box<dyn Element>,
    focused: bool,
}

impl Element for KeyCatcher {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        self.child.layout(constraint, ctx, app)
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app);
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.child.paint(origin, ctx, app);
    }

    fn size(&self) -> Option<Vector2F> {
        self.child.size()
    }

    fn origin(&self) -> Option<Point> {
        self.child.origin()
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        if self.child.dispatch_event(event, ctx, app) {
            return true;
        }
        if !self.focused {
            return false;
        }
        match event.raw_event() {
            Event::TypedCharacters { chars } => {
                ctx.dispatch_typed_action(TextFieldAction::Insert(chars.clone()));
                true
            }
            Event::KeyDown { keystroke, .. } => {
                let action = match (keystroke.cmd || keystroke.ctrl, keystroke.key.as_str()) {
                    (false, "backspace") => Some(TextFieldAction::Backspace),
                    (false, "enter") => Some(TextFieldAction::Submit),
                    (true, "v") => Some(TextFieldAction::Paste),
                    _ => None,
                };
                match action {
                    Some(action) => {
                        ctx.dispatch_typed_action(action);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }
}
