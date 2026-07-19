//! The template bank.
//!
//! Each template declares which phase it belongs to and which command classes
//! it is topical for. `classes: &[]` means "any class". Templates requiring a
//! slot the context can't fill are skipped automatically by
//! [`super::grammar::expand`], so it is safe to write `{DURATION}` lines here
//! and let the submit phase filter them out on its own.
//!
//! Two voices, deliberately distinct:
//! * [`Phase::Submit`] — weary anticipation. Present tense. It has seen the
//!   command and is already disappointed.
//! * [`Phase::Complete`] — verdict. Past tense. It watched the thing happen.

use super::classify::CommandClass;
use super::MiseryContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Fired when the command is submitted, before output exists.
    Submit,
    /// Fired when the block completes, with exit code and timing available.
    Complete,
}

/// An outcome precondition. Slot-level checks catch missing *data*; this
/// catches wrong *tone* — an "and it failed" line firing on exit 0, or a
/// "{LINES} lines, you'll read four" line firing on no output at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    /// No precondition.
    Any,
    /// Only when the command succeeded.
    Success,
    /// Only when the command failed.
    Failure,
    /// Only when there is enough output for a remark about volume to land.
    HasOutput,
    /// Only when the command printed nothing.
    NoOutput,
}

/// Below this, a line about how much output there is just looks wrong.
const MEANINGFUL_OUTPUT_LINES: u32 = 3;

impl Cond {
    fn matches(self, ctx: &MiseryContext) -> bool {
        match self {
            Cond::Any => true,
            // `succeeded` is None during submit, when no outcome exists yet;
            // outcome-conditional lines must not fire then.
            Cond::Success => ctx.succeeded == Some(true),
            Cond::Failure => ctx.succeeded == Some(false),
            Cond::HasOutput => ctx.output_lines.is_some_and(|n| n >= MEANINGFUL_OUTPUT_LINES),
            Cond::NoOutput => ctx.output_lines == Some(0),
        }
    }
}

pub struct Template {
    pub text: &'static str,
    pub phase: Phase,
    /// Command classes this line is topical for. Empty means universal.
    pub classes: &'static [CommandClass],
    /// Outcome precondition.
    pub cond: Cond,
    /// Relative selection weight. Universal lines get a lower weight so
    /// topical ones win when both apply.
    pub weight: u32,
}

/// Unconditional template.
const fn t(
    text: &'static str,
    phase: Phase,
    classes: &'static [CommandClass],
    weight: u32,
) -> Template {
    Template {
        text,
        phase,
        classes,
        cond: Cond::Any,
        weight,
    }
}

/// Template with an outcome precondition.
const fn tc(
    text: &'static str,
    phase: Phase,
    classes: &'static [CommandClass],
    cond: Cond,
    weight: u32,
) -> Template {
    Template {
        text,
        phase,
        classes,
        cond,
        weight,
    }
}

use CommandClass::*;
use Phase::{Complete, Submit};

pub const TEMPLATES: &[Template] = &[
    // ── Submit, universal ────────────────────────────────────────────────
    t("{EXCLAIM}. {COMPLAINT}.", Submit, &[], 6),
    t("{EXCLAIM}, {AGAIN}.", Submit, &[], 6),
    t("{A_ADJECTIVE} little {NOUN_TASK}. delighted.", Submit, &[], 5),
    t("{CMD}. what a {ADJECTIVE} use of {TALENT}.", Submit, &[], 6),
    t(
        "the last time I was this disinterested, I was {ACTIVITY}.",
        Submit,
        &[],
        8,
    ),
    t(
        "I bet you've never even {NEVER_EVEN} {FILE}.",
        Submit,
        &[],
        7,
    ),
    t("{EXCLAIM}. {TALENT}, and you want {CMD}.", Submit, &[], 6),
    t("fine. but {COMPLAINT}.", Submit, &[], 4),
    t(
        "somewhere in {DIR} there is real work. this is not it.",
        Submit,
        &[],
        5,
    ),
    t("you type {CMD} like it means something.", Submit, &[], 5),
    t("{EXCLAIM}. and you call yourself a {NOUN_USER}.", Submit, &[], 5),
    t(
        "I'd rather be {ACTIVITY}. and I hate that.",
        Submit,
        &[],
        7,
    ),

    // ── Submit, topical ──────────────────────────────────────────────────
    t(
        "{EXCLAIM}. listing files. the pinnacle of computing.",
        Submit,
        &[Listing],
        10,
    ),
    t(
        "they are the same files as last time. I checked.",
        Submit,
        &[Listing],
        10,
    ),
    t(
        "you could just remember what's in {DIR}. like an adult.",
        Submit,
        &[Listing],
        9,
    ),
    t(
        "moving house again. {AGAIN}.",
        Submit,
        &[Navigation],
        10,
    ),
    t(
        "you don't know where you are, do you.",
        Submit,
        &[Navigation],
        10,
    ),
    t(
        "clearing the screen won't clear what you've done.",
        Submit,
        &[Clear],
        12,
    ),
    t(
        "hiding the evidence. bold.",
        Submit,
        &[Clear],
        11,
    ),
    t(
        "reading {FILE}. now? after everything?",
        Submit,
        &[Inspection],
        9,
    ),
    t(
        "searching. because you didn't write it down.",
        Submit,
        &[Search],
        9,
    ),
    t(
        "another commit for the archaeologists.",
        Submit,
        &[Vcs],
        9,
    ),
    t(
        "git will remember this. that's rather the point.",
        Submit,
        &[Vcs],
        9,
    ),
    t(
        "oh, actual work. don't strain yourself.",
        Submit,
        &[Build],
        10,
    ),
    t(
        "deleting things. finally, we agree on something.",
        Submit,
        &[Destructive],
        11,
    ),
    t(
        "I hope you meant that.",
        Submit,
        &[Destructive],
        11,
    ),
    t(
        "installing more of other people's mistakes.",
        Submit,
        &[PackageMgmt],
        10,
    ),
    t(
        "opening an editor. the typing will be the easy part.",
        Submit,
        &[Editor],
        9,
    ),
    t(
        "talking to another machine. it won't like you either.",
        Submit,
        &[Network],
        9,
    ),
    t(
        "looking for something to blame. it's you.",
        Submit,
        &[Process],
        9,
    ),

    // ── Complete, universal ──────────────────────────────────────────────
    t("{VERDICT}.", Complete, &[], 6),
    t("{VERDICT}. {COMPLAINT}.", Complete, &[], 5),
    t("{EXCLAIM}. {VERDICT}.", Complete, &[], 5),
    t("{DURATION} for that. {COMPLAINT}.", Complete, &[], 7),
    t("you waited {DURATION}. I waited {DURATION}.", Complete, &[], 6),
    tc(
        "{LINES} lines. you'll read four of them.",
        Complete,
        &[],
        Cond::HasOutput,
        7,
    ),
    tc(
        "{LINES} lines of output, and you'll scroll past all of it.",
        Complete,
        &[],
        Cond::HasOutput,
        6,
    ),
    tc(
        "done in {DURATION}. I spent it {ACTIVITY}.",
        Complete,
        &[],
        Cond::Success,
        9,
    ),
    t("{VERDICT}. and you'll do it again in a minute.", Complete, &[], 5),
    tc(
        "nothing. it returned nothing. fitting.",
        Complete,
        &[],
        Cond::NoOutput,
        6,
    ),
    tc(
        "not a single line of output. {COMPLAINT}.",
        Complete,
        &[],
        Cond::NoOutput,
        5,
    ),

    // ── Complete, failure-specific ───────────────────────────────────────
    tc(
        "exit {EXIT}. I'd explain, but you wouldn't read that either.",
        Complete,
        &[],
        Cond::Failure,
        8,
    ),
    tc(
        "exit {EXIT}. after {DURATION}. exquisite.",
        Complete,
        &[],
        Cond::Failure,
        8,
    ),
    tc(
        "it failed, and I watched every {DURATION} of it.",
        Complete,
        &[],
        Cond::Failure,
        7,
    ),
    tc(
        "broken. I'd rather be {ACTIVITY}.",
        Complete,
        &[],
        Cond::Failure,
        8,
    ),

    // ── Complete, topical ────────────────────────────────────────────────
    tc(
        "there. the same files. as I said.",
        Complete,
        &[Listing],
        Cond::Success,
        11,
    ),
    tc(
        "{LINES} filenames. you were looking for one.",
        Complete,
        &[Listing],
        Cond::HasOutput,
        11,
    ),
    tc(
        "you are now somewhere else. transformative.",
        Complete,
        &[Navigation],
        Cond::Success,
        11,
    ),
    tc(
        "a blank screen. much better. nothing has changed.",
        Complete,
        &[Clear],
        Cond::Success,
        12,
    ),
    tc(
        "built. in {DURATION}. I did all of it.",
        Complete,
        &[Build],
        Cond::Success,
        11,
    ),
    tc(
        "it didn't even build. after {DURATION}.",
        Complete,
        &[Build],
        Cond::Failure,
        12,
    ),
    tc(
        "it's gone. I hope that was the plan.",
        Complete,
        &[Destructive],
        Cond::Success,
        12,
    ),
    tc(
        "{LINES} matches. one of them is probably the one.",
        Complete,
        &[Search],
        Cond::HasOutput,
        10,
    ),
    tc(
        "nothing matched. it isn't there, and it never was.",
        Complete,
        &[Search],
        Cond::NoOutput,
        11,
    ),
];

/// All templates valid for `phase`, `class` and the current outcome.
pub fn candidates<'a>(
    phase: Phase,
    class: CommandClass,
    ctx: &'a MiseryContext,
) -> impl Iterator<Item = &'static Template> + 'a {
    TEMPLATES.iter().filter(move |t| {
        t.phase == phase
            && (t.classes.is_empty() || t.classes.contains(&class))
            && t.cond.matches(ctx)
    })
}
