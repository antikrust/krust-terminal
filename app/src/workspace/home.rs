//! Krust Home
//!
//! This is the landing page for new tabs if session creation isn't supported (e.g. on the web).
//! It's barebones at the moment, but may grow into a more full-featured admin experience.

use krustui::ViewContext;

use super::view::Workspace;
use crate::pane_group::{AnyPaneContent, FilePane};

const KRUST_HOME_TITLE: &str = "Welcome to Krust on Web";
const KRUST_HOME_CONTENT: &str = r#"
Welcome to Krust on Web - your browser-based home for Krust! 
Use Krust on Web to:
* Join Shared Sessions
* Create, View, and Edit Krust Drive Objects
* Manage your Krust Settings

Krust on Web can also be used by your teammates and peers who don't have Krust downloaded yet to view your shared sessions, notebooks, and workflows."#;

/// Create a static "home page" pane.
pub fn create_home_pane(ctx: &mut ViewContext<Workspace>) -> Box<dyn AnyPaneContent> {
    let pane = FilePane::new(
        None,
        None,
        #[cfg(feature = "local_fs")]
        None,
        ctx,
    );
    pane.file_view(ctx).update(ctx, |pane, ctx| {
        pane.open_static(KRUST_HOME_TITLE, KRUST_HOME_CONTENT, ctx);
    });
    Box::new(pane)
}
