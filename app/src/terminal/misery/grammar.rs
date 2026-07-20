//! Template expansion.
//!
//! Templates are plain strings containing `{SLOT}` placeholders. Expansion
//! walks the string once, resolving each slot against the [`MiseryContext`] and
//! an RNG. A slot that cannot be resolved (no filenames in an empty directory,
//! no duration on a submit-phase line) causes the whole expansion to fail, and
//! the caller picks a different template — this is what keeps the engine from
//! emitting "I was alphabetising  against ".

use rand::seq::SliceRandom;
use rand::Rng;

use super::{vocab, MiseryContext};

/// Expands `template`, or returns `None` if any slot in it can't be filled.
pub fn expand<R: Rng + ?Sized>(template: &str, ctx: &MiseryContext, rng: &mut R) -> Option<String> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;

    // Filenames are drawn without replacement so `{FILE}` twice in one line
    // never names the same file.
    let mut file_pool: Vec<&str> = ctx.nearby_files.iter().map(String::as_str).collect();
    file_pool.shuffle(rng);
    let mut file_cursor = 0usize;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let close = after_open.find('}')?;
        let slot = &after_open[..close];

        let value = resolve(slot, ctx, rng, &file_pool, &mut file_cursor)?;
        out.push_str(&value);

        rest = &after_open[close + 1..];
    }
    out.push_str(rest);

    Some(tidy_punctuation(&out))
}

/// Collapses doubled sentence punctuation.
///
/// Templates append their own full stop, but some slot values are whole
/// sentences that already end in one — `{VERDICT}.` with "there. happy?"
/// yields "there. happy?.". Rather than forbid punctuation in the vocabulary
/// (which would constrain the writing for a purely mechanical reason), fix it
/// on the way out.
fn tidy_punctuation(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        out.push(c);
        if matches!(c, '.' | '!' | '?') {
            // Drop any run of terminal punctuation immediately following, so
            // "?." becomes "?" and ".." becomes ".". An ellipsis is written as
            // a single '…' in the vocabulary, so this cannot eat one.
            while chars.peek().is_some_and(|n| matches!(n, '.' | '!' | '?')) {
                chars.next();
            }
        }
    }
    out
}

fn resolve<R: Rng + ?Sized>(
    slot: &str,
    ctx: &MiseryContext,
    rng: &mut R,
    file_pool: &[&str],
    file_cursor: &mut usize,
) -> Option<String> {
    // Nested slots: ACTIVITY expands to a gerund plus one or two filenames,
    // which is the trick that makes output sound researched.
    match slot {
        "EXCLAIM" => pick(exclaim_list(ctx), rng),
        "AGAIN" => pick(vocab::AGAIN, rng),
        "TALENT" => pick(vocab::TALENT, rng),
        "ADJECTIVE" => pick(vocab::ADJECTIVE, rng),
        // Adjective with its indefinite article already agreed, so templates
        // never produce "a inert little exercise".
        "A_ADJECTIVE" => {
            let adjective = pick(vocab::ADJECTIVE, rng)?;
            Some(format!("{} {adjective}", indefinite_article(&adjective)))
        }
        "NOUN_TASK" => pick(vocab::NOUN_TASK, rng),
        "NOUN_USER" => pick(noun_user_list(ctx), rng),
        "COMPLAINT" => pick(vocab::COMPLAINT, rng),
        "VERDICT" => {
            let list = if ctx.succeeded.unwrap_or(true) {
                vocab::VERDICT_SUCCESS
            } else {
                vocab::VERDICT_FAILURE
            };
            pick(list, rng)
        }
        "NEVER_EVEN" => pick(vocab::NEVER_EVEN, rng),
        "GERUND" => pick(vocab::GERUND, rng),
        "PREP" => pick(vocab::PREP, rng),

        // A synthesised activity: "cross-referencing `Cargo.toml` against
        // `README.md`". Needs at least one real filename.
        "ACTIVITY" => {
            let gerund = pick(vocab::GERUND, rng)?;
            let first = next_file(file_pool, file_cursor)?;
            // Use two files when we have them, most of the time.
            let use_two = file_pool.len() > *file_cursor && rng.gen_bool(0.6);
            if use_two {
                let prep = pick(vocab::PREP, rng)?;
                let second = next_file(file_pool, file_cursor)?;
                Some(format!("{gerund} `{first}` {prep} `{second}`"))
            } else {
                Some(format!("{gerund} `{first}`"))
            }
        }

        "FILE" => next_file(file_pool, file_cursor).map(|f| format!("`{f}`")),
        "CMD" => Some(format!("`{}`", ctx.command_name)),
        "DIR" => ctx.directory_name.clone().map(|d| format!("`{d}`")),

        "DURATION" => ctx.duration.map(format_duration),
        "LINES" => ctx.output_lines.map(|n| n.to_string()),
        "EXIT" => ctx.exit_code.map(|c| c.to_string()),

        "QUOTE" => quote_fragment(ctx, rng),

        _ => None,
    }
}

/// A short run of the user's agent prompt, flung back at them — "repeating a
/// bit of what you said to take the piss" rather than a full quote. Returns
/// `None` (which fails the whole template, so quote lines never surface)
/// unless there is a prompt to quote, i.e. unless we're in agent mode.
fn quote_fragment<R: Rng + ?Sized>(ctx: &MiseryContext, rng: &mut R) -> Option<String> {
    let words: Vec<&str> = ctx.user_prompt.as_deref()?.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let window = rng.gen_range(2..=5).min(words.len());
    let start = rng.gen_range(0..=words.len() - window);
    Some(words[start..start + window].join(" "))
}

fn exclaim_list(ctx: &MiseryContext) -> &'static [&'static str] {
    if ctx.swear_mode {
        vocab::EXCLAIM_SWEAR
    } else {
        vocab::EXCLAIM_CLEAN
    }
}

fn noun_user_list(ctx: &MiseryContext) -> &'static [&'static str] {
    if ctx.swear_mode {
        vocab::NOUN_USER_SWEAR
    } else {
        vocab::NOUN_USER_CLEAN
    }
}

fn pick<R: Rng + ?Sized>(list: &[&str], rng: &mut R) -> Option<String> {
    list.choose(rng).map(|s| (*s).to_owned())
}

fn next_file(pool: &[&str], cursor: &mut usize) -> Option<String> {
    let file = pool.get(*cursor)?;
    *cursor += 1;
    Some((*file).to_owned())
}

/// Picks "a" or "an" for `word`.
///
/// Spelling is a decent proxy for pronunciation but not a reliable one, so the
/// two classes where it lies get explicit lists: "yoo"-initial vowels take "a"
/// (a useful tool), and silent-h words take "an" (an hour).
fn indefinite_article(word: &str) -> &'static str {
    const SILENT_H: &[&str] = &["hour", "honest", "heir", "honour", "honor", "herb"];

    let lower = word.to_ascii_lowercase();

    if SILENT_H.iter().any(|w| lower.starts_with(w)) {
        return "an";
    }

    let starts_with_vowel = lower.starts_with(['a', 'e', 'i', 'o', 'u']);
    if !starts_with_vowel {
        return "a";
    }

    // "unique", "useful", "european" — vowel-spelled, consonant-sounding.
    // Matched on longer prefixes so genuinely vowel-sounding words like
    // "unimaginative" and "inert" are not caught.
    const YOO_WORDS: &[&str] = &[
        "unique",
        "useful",
        "user",
        "usual",
        "united",
        "universal",
        "universe",
        "unicorn",
        "unified",
        "uniform",
        "unilateral",
        "utility",
        "utopian",
        "eulogy",
        "european",
        "ukulele",
        "one",
    ];
    if YOO_WORDS.iter().any(|w| lower.starts_with(w)) {
        return "a";
    }

    "an"
}

/// Human-readable duration, tuned for contempt rather than precision.
fn format_duration(millis: u64) -> String {
    if millis < 1000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        let secs = millis as f64 / 1000.0;
        if secs < 10.0 {
            format!("{secs:.1} seconds")
        } else {
            format!("{} seconds", secs.round() as u64)
        }
    } else {
        let mins = millis / 60_000;
        let secs = (millis % 60_000) / 1000;
        if secs == 0 {
            format!("{mins} minutes")
        } else {
            format!("{mins}m{secs}s")
        }
    }
}

#[cfg(test)]
#[path = "grammar_tests.rs"]
mod tests;
