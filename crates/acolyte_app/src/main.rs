//! Acolyte, the desktop app: runs this machine's acolyte — the conduit that polls Moloch for work
//! and does it on the machine's Ollama models — and shows what it is doing, in Krust's frame
//! (`krust_app_shell`). Acolyte itself has no window or port; this starts and stops it with a
//! config of its own, and watches its output, Ollama, and whether Moloch sees it.

mod acolyte;
mod pages;

use krust_app_shell::Page;

fn main() -> anyhow::Result<()> {
    krust_app_shell::run("Acolyte", |ctx| {
        ctx.add_singleton_model(acolyte::Acolyte::new);
        vec![
            Page::view("Overview", ctx.add_typed_action_view(pages::Overview::new)),
            Page::view("Activity", ctx.add_typed_action_view(pages::Activity::new)),
            Page::view("Models", ctx.add_typed_action_view(pages::Models::new)),
            Page::view("Config", ctx.add_typed_action_view(pages::Config::new)),
        ]
    })
}
