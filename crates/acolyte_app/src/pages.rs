//! Acolyte's pages: Overview (start and stop it, and how it is doing), Activity (its output),
//! Models (what Ollama offers), Config. Each draws from the one [`Acolyte`] model.

use krust_app_shell::page;
use krust_app_shell::{TextField, TextFieldEvent};
use krust_core::ui::appearance::Appearance;
use krustui::elements::{
    ChildView, ClippedScrollStateHandle, ConstrainedBox, Element, MouseStateHandle,
};
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::UiComponent;
use krustui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use crate::acolyte::{self, Acolyte};

#[derive(Clone, Debug, PartialEq)]
pub enum AcolyteAction {
    Start,
    Stop,
    Check,
    Save,
}

/// Lines of output the Activity page shows, the newest last.
const SHOWN_LINES: usize = 400;

fn follow<V: View>(ctx: &mut ViewContext<V>) {
    ctx.subscribe_to_model(&Acolyte::handle(ctx), |_, _, _, ctx| ctx.notify());
}

fn handle_common<V: View>(action: &AcolyteAction, ctx: &mut ViewContext<V>) {
    match action {
        AcolyteAction::Start => Acolyte::handle(ctx).update(ctx, |acolyte, ctx| acolyte.start(ctx)),
        AcolyteAction::Stop => Acolyte::handle(ctx).update(ctx, |acolyte, ctx| acolyte.stop(ctx)),
        AcolyteAction::Check => Acolyte::handle(ctx).update(ctx, |acolyte, ctx| acolyte.check(ctx)),
        // The Config page's own.
        AcolyteAction::Save => {}
    }
}

fn button(label: &str, action: AcolyteAction, mouse: &MouseStateHandle, appearance: &Appearance) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .button(ButtonVariant::Secondary, mouse.clone())
        .with_text_label(label.to_owned())
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

// ---------------------------------------------------------------------------

pub struct Overview {
    scroll: ClippedScrollStateHandle,
    run: MouseStateHandle,
    check: MouseStateHandle,
}

impl Overview {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
            run: Default::default(),
            check: Default::default(),
        }
    }
}

impl Entity for Overview {
    type Event = ();
}

impl View for Overview {
    fn ui_name() -> &'static str {
        "AcolyteOverview"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let acolyte = Acolyte::as_ref(app);
        let config = Acolyte::config();
        let setting = |key: &str, unset: &str| {
            config
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| unset.to_owned())
        };
        let state = match acolyte.pid() {
            Some(pid) => format!("running (pid {pid})"),
            None => acolyte
                .last_exit
                .clone()
                .map_or_else(|| "not running".to_owned(), |exit| format!("not running — {exit}")),
        };
        let moloch = match (acolyte.connected, &acolyte.moloch_error) {
            (Some(true), _) => "lists this acolyte as connected".to_owned(),
            (Some(false), _) => "answers, but doesn't list this acolyte yet".to_owned(),
            (None, Some(error)) => error.clone(),
            (None, None) => "checking…".to_owned(),
        };
        let ollama = match (&acolyte.ollama_models, &acolyte.ollama_error) {
            (Some(models), _) => format!("{} models", models.len()),
            (None, Some(error)) => error.clone(),
            (None, None) => "checking…".to_owned(),
        };
        let mut body = vec![
            page::note(
                "Acolyte is the conduit that serves Moloch: it asks Moloch for work and does it on this \
                 machine's Ollama models. It runs while this app is open.",
                appearance,
            ),
            page::row("Acolyte", &state, appearance),
            page::row("Worker name", &Acolyte::name(), appearance),
            page::row(
                "Moloch",
                &format!("{} — {moloch}", setting("MOLOCH_URL", "no address set")),
                appearance,
            ),
            page::row(
                "Ollama",
                &format!("{} — {ollama}", setting("OLLAMA_URL", "http://127.0.0.1:11434")),
                appearance,
            ),
            page::row("Config", &acolyte::config_path().display().to_string(), appearance),
        ];
        body.push(page::labelled(
            "",
            if acolyte.is_running() {
                button("Stop", AcolyteAction::Stop, &self.run, appearance)
            } else {
                button("Start", AcolyteAction::Start, &self.run, appearance)
            },
            appearance,
        ));
        let links = vec![page::link("Check", AcolyteAction::Check, &self.check, appearance)];
        page::frame("Overview", links, body, &self.scroll, appearance)
    }
}

impl TypedActionView for Overview {
    type Action = AcolyteAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Activity {
    scroll: ClippedScrollStateHandle,
}

impl Activity {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
        }
    }
}

impl Entity for Activity {
    type Event = ();
}

impl View for Activity {
    fn ui_name() -> &'static str {
        "AcolyteActivity"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let lines = Acolyte::as_ref(app).lines();
        let shown = &lines[lines.len().saturating_sub(SHOWN_LINES)..];
        let body = vec![if shown.is_empty() {
            page::note("Nothing yet — start acolyte on the Overview page.", appearance)
        } else {
            page::mono_lines(shown, appearance)
        }];
        page::frame("Activity", Vec::new(), body, &self.scroll, appearance)
    }
}

impl TypedActionView for Activity {
    type Action = AcolyteAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Models {
    scroll: ClippedScrollStateHandle,
    check: MouseStateHandle,
}

impl Models {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        follow(ctx);
        Self {
            scroll: Default::default(),
            check: Default::default(),
        }
    }
}

impl Entity for Models {
    type Event = ();
}

impl View for Models {
    fn ui_name() -> &'static str {
        "AcolyteModels"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let acolyte = Acolyte::as_ref(app);
        let mut body = vec![page::note(
            "Every model this machine's Ollama has — acolyte offers them all to Moloch, and picks up a \
             newly pulled one without restarting.",
            appearance,
        )];
        match (&acolyte.ollama_models, &acolyte.ollama_error) {
            (Some(models), _) if models.is_empty() => {
                body.push(page::note("Ollama has no models.", appearance));
            }
            (Some(models), _) => body.push(page::mono_lines(models, appearance)),
            (None, Some(error)) => body.push(page::note(&format!("Ollama: {error}."), appearance)),
            (None, None) => body.push(page::note("Checking…", appearance)),
        }
        let links = vec![page::link("Check", AcolyteAction::Check, &self.check, appearance)];
        page::frame("Models", links, body, &self.scroll, appearance)
    }
}

impl TypedActionView for Models {
    type Action = AcolyteAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        handle_common(action, ctx);
    }
}

// ---------------------------------------------------------------------------

pub struct Config {
    scroll: ClippedScrollStateHandle,
    fields: Vec<(&'static str, &'static str, ViewHandle<TextField>)>,
    save: MouseStateHandle,
    message: Option<String>,
}

impl Config {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let config = Acolyte::config();
        let fields: Vec<_> = acolyte::KEYS
            .iter()
            .map(|(key, label)| {
                let value = config.get(*key).cloned().unwrap_or_default();
                let masked = *key == "MOLOCH_TOKEN";
                let placeholder = match *key {
                    "MOLOCH_URL" => "http://host:10102",
                    "MOLOCH_TOKEN" => "Token or API key (wk-…)",
                    "OLLAMA_URL" => "http://127.0.0.1:11434",
                    "ACOLYTE_NAME" => "this machine's hostname",
                    "ACOLYTE_KINDS" => "any job",
                    "ACOLYTE_MODEL" => "the first installed",
                    _ => "the one bundled with this app",
                };
                let handle = ctx.add_typed_action_view(move |_| TextField::new(&value, placeholder, masked));
                (*key, *label, handle)
            })
            .collect();
        for (_, _, handle) in &fields {
            ctx.subscribe_to_view(handle, |me, _, event, ctx| {
                if *event == TextFieldEvent::Submitted {
                    me.save(ctx);
                }
            });
        }
        Self {
            scroll: Default::default(),
            fields,
            save: Default::default(),
            message: None,
        }
    }

    fn save(&mut self, ctx: &mut ViewContext<Self>) {
        let values: Vec<(&str, String)> = self
            .fields
            .iter()
            .map(|(key, _, handle)| (*key, handle.as_ref(ctx).text().trim().to_owned()))
            .collect();
        let running = Acolyte::as_ref(ctx).is_running();
        let result = Acolyte::handle(ctx).update(ctx, |acolyte, ctx| acolyte.save_config(&values, ctx));
        self.message = Some(match result {
            Ok(()) if running => "Saved. Stop and start acolyte for it to use the new config.".to_owned(),
            Ok(()) => "Saved.".to_owned(),
            Err(error) => format!("Couldn't save: {error}"),
        });
        ctx.notify();
    }
}

impl Entity for Config {
    type Event = ();
}

impl View for Config {
    fn ui_name() -> &'static str {
        "AcolyteConfig"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut body = vec![page::note(
            "Acolyte's settings, kept in this app's own file and handed to acolyte when it starts. \
             Empty ones take acolyte's defaults.",
            appearance,
        )];
        for (_, label, handle) in &self.fields {
            body.push(page::labelled(
                label,
                ConstrainedBox::new(ChildView::new(handle).finish())
                    .with_width(380.)
                    .finish(),
                appearance,
            ));
        }
        body.push(page::labelled(
            "",
            button("Save", AcolyteAction::Save, &self.save, appearance),
            appearance,
        ));
        if let Some(message) = &self.message {
            body.push(page::note(message, appearance));
        }
        page::frame("Config", Vec::new(), body, &self.scroll, appearance)
    }
}

impl TypedActionView for Config {
    type Action = AcolyteAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AcolyteAction::Save => self.save(ctx),
            other => handle_common(other, ctx),
        }
    }
}
