//! Turns a whole-checklist report into the conversation's todo operations.
//!
//! Claude's `TodoWrite`, Codex's `todo_list` and the local `todo_list` tool
//! all state the whole list every time, each item done or not. The
//! conversation's checklist is changed by operations instead: start a list,
//! replace its pending items, tick items off. A [`PlanTracker`] remembers the
//! list as last stated in this turn and emits only what changed, so a turn's
//! first report starts a new checklist and later ones tick items off it.

use std::collections::HashSet;

use ai::agent::{PlanEntry, PlanItem, PlanUpdate, TurnEvent};

#[derive(Default)]
pub(crate) struct PlanTracker {
    /// Whether this turn has started its checklist.
    listed: bool,
    /// The checklist's pending item ids, in order.
    pending: Vec<String>,
    /// Ids already ticked off. An item once done stays done.
    done: HashSet<String>,
}

impl PlanTracker {
    /// Passes `event` on, replacing a [`PlanUpdate::Snapshot`] with the
    /// operations it amounts to. For a harness whose parser reports snapshots
    /// (Claude, Codex): wrap the event sink in this.
    pub(crate) fn forward(&mut self, event: TurnEvent, on_event: &mut impl FnMut(TurnEvent)) {
        match event {
            TurnEvent::PlanUpdated {
                update: PlanUpdate::Snapshot(entries),
            } => {
                for update in self.updates(&entries) {
                    on_event(TurnEvent::PlanUpdated { update });
                }
            }
            event => on_event(event),
        }
    }

    /// The pending items, in order — the steps still to do. Empty until a
    /// checklist has been stated this turn.
    pub(crate) fn remaining(&self) -> Vec<String> {
        self.pending.clone()
    }

    /// The operations that bring the checklist to `entries`.
    pub(crate) fn updates(&mut self, entries: &[PlanEntry]) -> Vec<PlanUpdate> {
        let items = with_ids(entries);
        if !self.listed {
            if items.is_empty() {
                return Vec::new();
            }
            self.listed = true;
            let done: Vec<String> = items
                .iter()
                .filter(|(_, done)| *done)
                .map(|(item, _)| item.id.clone())
                .collect();
            self.pending = items
                .iter()
                .filter(|(_, done)| !*done)
                .map(|(item, _)| item.id.clone())
                .collect();
            self.done = done.iter().cloned().collect();
            let mut updates = vec![PlanUpdate::Create(
                items.into_iter().map(|(item, _)| item).collect(),
            )];
            if !done.is_empty() {
                updates.push(PlanUpdate::Complete(done));
            }
            return updates;
        }

        // Everything not ticked off yet is pending; an item done for the
        // first time is ticked off from there, so it must be pending first.
        let open: Vec<PlanItem> = items
            .iter()
            .filter(|(item, _)| !self.done.contains(&item.id))
            .map(|(item, _)| item.clone())
            .collect();
        let newly_done: Vec<String> = items
            .iter()
            .filter(|(item, done)| *done && !self.done.contains(&item.id))
            .map(|(item, _)| item.id.clone())
            .collect();
        let mut updates = Vec::new();
        if open.iter().map(|item| &item.id).ne(self.pending.iter()) {
            updates.push(PlanUpdate::UpdatePending(open.clone()));
        }
        if !newly_done.is_empty() {
            updates.push(PlanUpdate::Complete(newly_done.clone()));
        }
        self.done.extend(newly_done);
        self.pending = open
            .into_iter()
            .map(|item| item.id)
            .filter(|id| !self.done.contains(id))
            .collect();
        updates
    }
}

/// The entries as checklist items. An item's id is its title, so it stays
/// the same from one report to the next; a repeated title gets a number.
fn with_ids(entries: &[PlanEntry]) -> Vec<(PlanItem, bool)> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .filter(|entry| !entry.title.trim().is_empty())
        .map(|entry| {
            let title = entry.title.trim().to_string();
            let mut id = title.clone();
            let mut repeat = 1;
            while !seen.insert(id.clone()) {
                repeat += 1;
                id = format!("{title} ({repeat})");
            }
            (
                PlanItem {
                    id,
                    title,
                    description: entry.description.clone(),
                },
                entry.done,
            )
        })
        .collect()
}
