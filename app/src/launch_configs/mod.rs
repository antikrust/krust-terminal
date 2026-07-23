pub mod launch_config;
pub mod save_modal;

use derpui::AppContext;

pub fn init(app: &mut AppContext) {
    save_modal::init(app);
}
