//! Turns a whole-checklist report into the conversation's todo operations.
//!
//! Claude's `TodoWrite`, Codex's `todo_list` and the local `todo_list` tool
//! all state the whole list every time, each item done or not. The
//! conversation's checklist is changed by operations instead: start a list,
//! replace its pending items, tick items off. A [`PlanTracker`] remembers the
//! list as last stated in this turn and emits only what changed, so a turn's
//! first report starts a new checklist and later ones tick items off it.

use std::collections::{HashMap, HashSet};

use ai::agent::{PlanEntry, PlanItem, PlanUpdate, TurnEvent};

#[derive(Default)]
pub(crate) struct PlanTracker {
    /// Whether this turn has started its checklist.
    listed: bool,
    /// The checklist's pending item ids, in order.
    pending: Vec<String>,
    /// Ids already ticked off. An item once done stays done.
    done: HashSet<String>,
    /// Each item's id by its key (see [`item_key`]), so an item the model
    /// rewords from one report to the next — "(done)" tacked on, the detail
    /// after "Step 3:" changed — keeps the id it was first listed with,
    /// instead of reading as a new item while the old one shows cancelled.
    ids_by_key: HashMap<String, String>,
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
        let items = self.with_ids(entries);
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

impl PlanTracker {
    /// The entries as checklist items, with whether each is done. An item's
    /// id is its title as first listed; a later report's item with the same
    /// key keeps that id. A repeated title gets a number. Status marks a
    /// model writes into a title ("(done)", "✓", "(in progress)") are taken
    /// off it, and a done mark ticks the item off.
    fn with_ids(&mut self, entries: &[PlanEntry]) -> Vec<(PlanItem, bool)> {
        let mut seen = HashSet::new();
        entries
            .iter()
            .filter_map(|entry| {
                let (title, marked_done) = without_status_marks(&entry.title);
                (!title.is_empty()).then_some((entry, title, marked_done))
            })
            .map(|(entry, title, marked_done)| {
                let key = item_key(&title);
                let mut id = self
                    .ids_by_key
                    .get(&key)
                    .filter(|id| !seen.contains(*id))
                    .cloned()
                    .unwrap_or_else(|| title.clone());
                let mut repeat = 1;
                while !seen.insert(id.clone()) {
                    repeat += 1;
                    id = format!("{title} ({repeat})");
                }
                self.ids_by_key.entry(key).or_insert_with(|| id.clone());
                (
                    PlanItem {
                        id,
                        title,
                        description: entry.description.clone(),
                    },
                    entry.done || marked_done,
                )
            })
            .collect()
    }
}

/// Marks a model puts in a title to say an item is finished.
const DONE_MARKS: &[&str] = &[
    "(done)",
    "(completed)",
    "(complete)",
    "(finished)",
    "[done]",
    "[x]",
    "✓",
    "✔",
    "✅",
];

/// Marks a model puts in a title to say where an item stands, other than done.
const STATUS_MARKS: &[&str] = &[
    "(in progress)",
    "(in-progress)",
    "(current)",
    "(wip)",
    "(next)",
    "(pending)",
    "(todo)",
    "[ ]",
];

/// `title` without the status marks at its start or end, and whether one of
/// them said done.
fn without_status_marks(title: &str) -> (String, bool) {
    let starts_with = |text: &str, mark: &str| {
        text.get(..mark.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(mark))
    };
    let ends_with = |text: &str, mark: &str| {
        text.len() >= mark.len()
            && text
                .get(text.len() - mark.len()..)
                .is_some_and(|end| end.eq_ignore_ascii_case(mark))
    };
    let mut title = title.trim().to_string();
    let mut done = false;
    loop {
        let mark = DONE_MARKS
            .iter()
            .map(|mark| (*mark, true))
            .chain(STATUS_MARKS.iter().map(|mark| (*mark, false)))
            .find(|(mark, _)| starts_with(&title, mark) || ends_with(&title, mark));
        let Some((mark, is_done)) = mark else {
            break;
        };
        done |= is_done;
        title = if starts_with(&title, mark) {
            title[mark.len()..].to_string()
        } else {
            title[..title.len() - mark.len()].to_string()
        };
        title = title.trim().trim_end_matches(['-', ':', '—']).trim().to_string();
    }
    (title, done)
}

/// What identifies an item across reports: its step number when it is
/// written "Step 3: …" (so rewording the rest keeps it the same item), else
/// its title in lower case with spacing and trailing punctuation evened out.
fn item_key(title: &str) -> String {
    let lower = title.to_lowercase();
    if let Some(rest) = lower.strip_prefix("step") {
        let rest = rest.trim_start();
        let number: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let after = &rest[number.len()..];
        if number.starts_with(|c: char| c.is_ascii_digit())
            && (after.is_empty() || after.starts_with([':', '.', ')', ' ', '-']))
        {
            return format!("step {number}");
        }
    }
    lower
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', ':', ';', ','])
        .to_string()
}
