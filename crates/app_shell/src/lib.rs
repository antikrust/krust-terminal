//! The skeleton of Krust's desktop app, for apps of their own built on it — Moloch and Acolyte.
//!
//! It is Krust's startup and window frame with everything else taken out: the KrustUI app builder,
//! Krust's fonts and your Krust theme ([`look`]), and a window in Krust's frame ([`window`]) — the
//! black 28px title bar with the same close / minimise / fullscreen buttons on the left and your
//! identity avatar on the right, over a left bar of pages. An app supplies its name and its pages;
//! no terminal, session, settings store or agent code comes with it.
//!
//! ```ignore
//! krust_app_shell::run("Moloch", |ctx| vec![Page::view("Overview", ctx.add_typed_action_view(Overview::new))]);
//! ```

mod assets;
mod avatar;
mod dicebear;
pub mod env_file;
pub mod look;
mod menus;
pub mod page;
pub mod tabs;
pub mod text_field;
pub mod window;

pub use tabs::Tabs;
pub use text_field::{TextField, TextFieldEvent};
pub use window::{Page, ShellWindow};

use krustui::platform;
use krustui::{AddWindowOptions, ViewContext};

/// Runs an app called `name` in Krust's frame. `build_pages` makes its pages, in the window's
/// context, in the left bar's order. Returns when the app quits.
pub fn run<F>(name: &'static str, build_pages: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut ViewContext<ShellWindow>) -> Vec<Page> + 'static,
{
    #[allow(unused_mut)]
    let mut app_builder = platform::AppBuilder::new(
        platform::AppCallbacks::default(),
        Box::new(assets::ASSETS),
        None,
    );
    #[cfg(target_os = "macos")]
    {
        use krustui::platform::mac::AppExt;
        app_builder.set_activate_on_launch(true);
        app_builder.set_menu_bar_builder(move |_| menus::menu_bar(name));
    }
    let _ = app_builder.run(move |ctx| {
        if let Err(error) = look::init(ctx) {
            log::error!("{name}: could not load Krust's fonts: {error:#}");
        }
        window::init(ctx);
        ctx.add_singleton_model(|_| Tabs::default());
        let (window_id, _) = ctx.add_window(
            AddWindowOptions {
                title: Some(name.to_owned()),
                ..Default::default()
            },
            move |ctx| ShellWindow::new(name, build_pages, ctx),
        );
        window::style_window(window_id, ctx);
    });
    Ok(())
}
