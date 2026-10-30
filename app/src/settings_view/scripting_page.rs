//! Settings UI for local scripting and Krust control permissions.
use std::cell::RefCell;
use std::collections::HashMap;

use settings::Setting as _;
use krust_errors::report_if_error;
use krustui::elements::{Element, MouseStateHandle};
use krustui::ui_components::components::UiComponent;
use krustui::ui_components::switch::SwitchStateHandle;
use krustui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    render_body_item, LocalOnlyIconState, MatchData, PageType, SettingsPageMeta,
    SettingsPageViewHandle, SettingsWidget,
};
use super::{SettingsSection, ToggleState};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::settings::{LocalControlMode, LocalControlModeSetting, LocalControlSettings};

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptingSettingsPageAction {
    ToggleLocalControl,
}

pub struct ScriptingSettingsPageView {
    page: PageType<Self>,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
    local_control_switch: SwitchStateHandle,
}

impl ScriptingSettingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        if FeatureFlag::KrustControlCli.is_enabled() {
            ctx.subscribe_to_model(&LocalControlSettings::handle(ctx), |_, _, _, ctx| {
                ctx.notify();
            });
        }

        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> =
            vec![Box::new(LocalControlModeWidget)];

        Self {
            page: PageType::new_uncategorized(widgets, Some("Scripting")),
            local_only_icon_tooltip_states: RefCell::new(HashMap::new()),
            local_control_switch: SwitchStateHandle::default(),
        }
    }
}

impl Entity for ScriptingSettingsPageView {
    type Event = ();
}

impl TypedActionView for ScriptingSettingsPageView {
    type Action = ScriptingSettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ScriptingSettingsPageAction::ToggleLocalControl => {
                let mode = if LocalControlSettings::as_ref(ctx).is_enabled() {
                    LocalControlMode::Disabled
                } else {
                    LocalControlMode::Enabled
                };
                LocalControlSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_control_mode.set_value(mode, ctx));
                });
                ctx.notify();
            }
        }
    }
}

impl View for ScriptingSettingsPageView {
    fn ui_name() -> &'static str {
        "ScriptingSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for ScriptingSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Scripting
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        cfg!(not(target_family = "wasm")) && FeatureFlag::KrustControlCli.is_enabled()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<ScriptingSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<ScriptingSettingsPageView>) -> Self {
        SettingsPageViewHandle::Scripting(view_handle)
    }
}

struct LocalControlModeWidget;

impl SettingsWidget for LocalControlModeWidget {
    type View = ScriptingSettingsPageView;

    fn search_terms(&self) -> &str {
        "scripting krust control automation krustctrl local cli scripts disabled enabled"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let enabled = LocalControlSettings::as_ref(app).is_enabled();
        let switch = appearance
            .ui_builder()
            .switch(view.local_control_switch.clone())
            .check(enabled)
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(ScriptingSettingsPageAction::ToggleLocalControl);
            })
            .finish();
        render_body_item::<ScriptingSettingsPageAction>(
            "krustctrl CLI".into(),
            None,
            LocalOnlyIconState::for_setting(
                LocalControlModeSetting::storage_key(),
                LocalControlModeSetting::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
            switch,
            Some(
                "krustctrl is built into Krust. When on, scripts running as you can control \
                 Krust's UI and start agent instructions. Use with care."
                    .to_owned(),
            ),
        )
    }
}
