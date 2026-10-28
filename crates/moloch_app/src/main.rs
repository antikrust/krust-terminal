//! Moloch, the desktop app: your Moloch server — its agents (and Krust's local ones), acolytes,
//! live sessions, leaderboard and log, each agent's output in a tab — in Krust's frame
//! (`krust_app_shell`). It reads and writes the same connection Krust uses (`moloch.env` in
//! Krust's state folder), so both always point at the same server.

mod moloch;
mod pages;

use krust_app_shell::Page;

fn main() -> anyhow::Result<()> {
    krust_app_shell::run("Moloch", |ctx| {
        ctx.add_singleton_model(moloch::Moloch::new);
        vec![
            Page::view("Overview", ctx.add_typed_action_view(pages::Overview::new)),
            Page::view("Agents", ctx.add_typed_action_view(pages::Agents::new)),
            Page::view("Acolytes", ctx.add_typed_action_view(pages::Acolytes::new)),
            Page::view("Live sessions", ctx.add_typed_action_view(pages::Sessions::new)),
            Page::view("Leaderboard", ctx.add_typed_action_view(pages::Leaderboard::new)),
            Page::view("Logs", ctx.add_typed_action_view(pages::Logs::new)),
            Page::view("Connection", ctx.add_typed_action_view(pages::Connection::new)),
        ]
    })
}
