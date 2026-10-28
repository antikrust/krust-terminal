//! Tabs at the top of the window's content area, for things opened from a page — an agent's
//! output, say. The first tab is always the page picked in the left bar; the rest are what pages
//! opened, each with a × to close it. Any page opens one through this model:
//!
//! ```ignore
//! Tabs::handle(ctx).update(ctx, |tabs, ctx| tabs.open("run:42", "Fix the build", Page::view(…), ctx));
//! ```

use krustui::elements::{Element, MouseStateHandle};
use krustui::{Entity, ModelContext, SingletonEntity};

use crate::window::Page;

pub struct Tab {
    /// What it shows, so opening the same thing again picks its tab instead of adding one.
    pub key: String,
    pub title: String,
    page: Page,
    pub(crate) select: MouseStateHandle,
    pub(crate) close: MouseStateHandle,
}

impl Tab {
    pub(crate) fn render(&self) -> Box<dyn Element> {
        self.page.render()
    }
}

#[derive(Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    /// The tab shown; `None` is the left bar's page.
    selected: Option<usize>,
    /// The page tab's mouse state.
    pub(crate) page_tab: MouseStateHandle,
}

pub enum TabsEvent {
    Changed,
}

impl Entity for Tabs {
    type Event = TabsEvent;
}

impl SingletonEntity for Tabs {}

impl Tabs {
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Opens a tab showing `page` under `title` and picks it — or picks the one already open for
    /// `key`.
    pub fn open(&mut self, key: &str, title: &str, page: Page, ctx: &mut ModelContext<Self>) {
        let index = match self.tabs.iter().position(|tab| tab.key == key) {
            Some(index) => index,
            None => {
                self.tabs.push(Tab {
                    key: key.to_owned(),
                    title: title.to_owned(),
                    page,
                    select: Default::default(),
                    close: Default::default(),
                });
                self.tabs.len() - 1
            }
        };
        self.selected = Some(index);
        ctx.emit(TabsEvent::Changed);
    }

    /// Picks a tab; `None` goes back to the left bar's page.
    pub fn select(&mut self, index: Option<usize>, ctx: &mut ModelContext<Self>) {
        self.selected = index.filter(|index| *index < self.tabs.len());
        ctx.emit(TabsEvent::Changed);
    }

    /// Closes a tab, picking the one beside it (or the page) if it was shown.
    pub fn close(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.selected = match self.selected {
            Some(selected) if selected == index => {
                if self.tabs.is_empty() {
                    None
                } else {
                    Some(index.min(self.tabs.len() - 1))
                }
            }
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };
        ctx.emit(TabsEvent::Changed);
    }
}
