use settings::Setting as _;
use krust_errors::report_if_error;
use krustui::presenter::ChildView;
use krustui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::{TextEffect, TextEffectsSettings};
use crate::menu::MenuVariant;
use crate::ui_components::icons::Icon;
use crate::view_components::{CompactDropdown, CompactDropdownEvent, CompactDropdownItem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEffectPickerAction {
    Select(Option<TextEffect>),
}

pub enum TextEffectPickerEvent {
    Close,
}

pub struct TextEffectPicker {
    dropdown: ViewHandle<CompactDropdown<TextEffectPickerAction>>,
}

impl TextEffectPicker {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = CompactDropdown::new(MenuVariant::Fixed, ctx);
            let items = std::iter::once(CompactDropdownItem::new(
                Icon::SlashCircle,
                "Off",
                TextEffectPickerAction::Select(None),
            ))
            .chain(TextEffect::VALUES.into_iter().map(|effect| {
                CompactDropdownItem::new(
                    effect_icon(effect),
                    effect.label(),
                    TextEffectPickerAction::Select(Some(effect)),
                )
            }));
            dropdown.set_items(items, ctx);
            dropdown.set_selected_by_name(selected_effect_label(ctx), ctx);
            dropdown.set_icon_size(16.);
            dropdown.set_trigger_icon(Icon::CaseSensitivity);
            dropdown.set_show_trigger_chevron(false);
            dropdown
        });

        ctx.subscribe_to_view(&dropdown, |_, _, event, ctx| match event {
            CompactDropdownEvent::Close => ctx.emit(TextEffectPickerEvent::Close),
        });
        ctx.subscribe_to_model(&TextEffectsSettings::handle(ctx), |me, _, _, ctx| {
            me.dropdown.update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_name(selected_effect_label(ctx), ctx);
            });
            ctx.notify();
        });

        Self { dropdown }
    }
}

impl Entity for TextEffectPicker {
    type Event = TextEffectPickerEvent;
}

impl View for TextEffectPicker {
    fn ui_name() -> &'static str {
        "TextEffectPicker"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        ChildView::new(&self.dropdown).finish()
    }
}

impl TypedActionView for TextEffectPicker {
    type Action = TextEffectPickerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            TextEffectPickerAction::Select(effect) => {
                TextEffectsSettings::handle(ctx).update(ctx, |settings, ctx| match effect {
                    Some(effect) => {
                        report_if_error!(settings.effect.set_value(*effect, ctx));
                        report_if_error!(settings.enabled.set_value(true, ctx));
                    }
                    None => report_if_error!(settings.enabled.set_value(false, ctx)),
                });
            }
        }
    }
}

fn selected_effect_label(app: &AppContext) -> &'static str {
    let settings = TextEffectsSettings::as_ref(app);
    if *settings.enabled.value() {
        settings.effect.value().label()
    } else {
        "Off"
    }
}

fn effect_icon(effect: TextEffect) -> Icon {
    match effect {
        TextEffect::Immediate => Icon::FastForward,
        TextEffect::WordReveal => Icon::TextBlock,
        TextEffect::Typewriter => Icon::TerminalInput,
        TextEffect::MatrixTrail => Icon::Dataflow,
        TextEffect::Decode => Icon::Stars,
        TextEffect::CasePulse => Icon::CaseSensitivity,
        TextEffect::WordDecode => Icon::Regex,
    }
}
