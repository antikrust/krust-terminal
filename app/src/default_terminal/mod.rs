use borkui::windowing::{StateEvent, WindowManager};
use borkui::{Entity, ModelContext, ModelHandle, SingletonEntity};

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
use mac::*;

#[allow(dead_code)]
#[cfg(not(target_os = "macos"))]
mod non_mac {
    pub fn can_become_default_terminal() -> bool {
        false
    }

    pub fn is_bork_default_terminal() -> bool {
        false
    }

    /// Sets Bork as the default terminal
    pub fn set_bork_as_default_terminal() -> Result<(), String> {
        Err("Not implemented".to_string())
    }
}

#[allow(unused_imports)]
#[cfg(not(target_os = "macos"))]
use non_mac::*;
use bork_errors::report_error;

pub struct DefaultTerminal {
    /// Whether the OS will treat Bork as the default app for scripts/executables.
    is_bork_default: bool,
}

impl DefaultTerminal {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(
            &WindowManager::handle(ctx),
            Self::handle_window_manager_event,
        );

        // This can be slow to compute due to calling into platform APIs, so in unit
        // tests, where we shouldn't care, just pretend that we are not.
        let is_bork_default = if cfg!(test) {
            false
        } else {
            is_bork_default_terminal()
        };

        Self { is_bork_default }
    }

    /// This is an OS-level setting. Unlike most other settings, where Bork is the source-of-truth
    /// for the value of the setting, it can be changed outside of Bork. We monitor if it gets
    /// changed externally by checking when Bork is focused.
    fn handle_window_manager_event(
        &mut self,
        _: ModelHandle<WindowManager>,
        event: &StateEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            StateEvent::ValueChanged { current, previous } => {
                if current.active_window.is_some() && previous.active_window.is_none() {
                    let is_bork_default_now = is_bork_default_terminal();
                    if is_bork_default_now != self.is_bork_default {
                        self.set_is_bork_default(is_bork_default_now, ctx);
                    }
                }
            }
        }
    }

    fn set_is_bork_default(&mut self, value: bool, ctx: &mut ModelContext<Self>) {
        self.is_bork_default = value;
        ctx.emit(DefaultTerminalEvent::ValueChanged);
        ctx.notify();
    }

    pub fn can_bork_become_default() -> bool {
        if cfg!(test) {
            // Determining whether or not we can become the default terminal requires
            // calling into platform APIs, which can be slow, and we can't actually
            // set the default terminal in unit tests, so just say we can't.
            false
        } else {
            can_become_default_terminal()
        }
    }

    pub fn is_bork_default(&self) -> bool {
        self.is_bork_default
    }

    /// This is a one-way operation. Once we set the default terminal to Bork, we can't really
    /// "unset" it unless we pick a new default terminal. Picking a new default is complicated.
    pub fn make_bork_default(&mut self, ctx: &mut ModelContext<Self>) {
        if let Err(e) = set_bork_as_default_terminal() {
            report_error!(
                "Error setting Bork as default terminal",
                extra: { "error" => %e }
            );
        } else {
            self.set_is_bork_default(true, ctx);
        }
    }
}

pub enum DefaultTerminalEvent {
    ValueChanged,
}

impl Entity for DefaultTerminal {
    type Event = DefaultTerminalEvent;
}

impl SingletonEntity for DefaultTerminal {}
