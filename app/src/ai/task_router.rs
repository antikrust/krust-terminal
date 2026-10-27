//! The decider: how hard a task is, and which model it should go to.
//!
//! Warp's custom routers only *define* buckets (easy, medium, hard → a model); the decision
//! itself, scoring a task, was made by their server. Here nothing decides, so this does, with no
//! model call and no user-written ratings:
//!
//! - [`decide`] reads the task: an explicit `difficulty: hard` line wins, otherwise signals in its
//!   text (length, how many files it names, words like "refactor" or "bit-identical" against
//!   "rename" or "typo") are added up, and the reasons are kept so a person can see why.
//! - [`strength`] scores a model from what its name and price say (its size in billions of
//!   parameters, a "pro" or a "mini", what a million tokens cost, how long a context it takes).
//!   Models are then ranked, and [`pick_rank`] takes the one nearest the rank the difficulty
//!   asks for: hard goes to the strongest available, easy to the weakest.
//! - A task that fails on its model is retried a tier up ([`Difficulty::harder`]), so a wrong
//!   guess about a model costs one retry, not the task.
//! - [`choose`] is the single entry point: a [`Policy`] (the "auto" flavours in the model
//!   picker, or the queue's default), a difficulty and the candidates, one answer. The same
//!   algorithm runs in the app and in Moloch; `docs/chooser-golden.json` holds inputs and the
//!   answers both must give.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

impl Difficulty {
    pub fn label(self) -> &'static str {
        match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        }
    }

    pub fn harder(self) -> Self {
        match self {
            Difficulty::Easy => Difficulty::Medium,
            Difficulty::Medium | Difficulty::Hard => Difficulty::Hard,
        }
    }

    /// Where on the ranking of available models (0 weakest, 1 strongest) this difficulty aims.
    fn target_rank(self) -> f64 {
        match self {
            Difficulty::Easy => 0.0,
            Difficulty::Medium => 0.5,
            Difficulty::Hard => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub difficulty: Difficulty,
    /// What decided it, for the task's row.
    pub reason: String,
}

/// Words that make a task harder, and easier, with how much.
const HARD_WORDS: &[(&str, i32)] = &[
    ("refactor", 2),
    ("architecture", 2),
    ("redesign", 2),
    ("debug", 1),
    ("investigate", 1),
    ("reverse engineer", 2),
    ("decompile", 1),
    ("bit-identical", 2),
    ("bit for bit", 2),
    ("exact copy", 2),
    ("algorithm", 1),
    ("concurrency", 2),
    ("race condition", 2),
    ("from scratch", 2),
    ("across the codebase", 2),
    ("every function", 1),
    ("prove", 1),
    ("optimi", 1),
    ("migrate", 1),
];
const EASY_WORDS: &[(&str, i32)] = &[
    ("rename", 2),
    ("typo", 2),
    ("one line", 2),
    ("add a comment", 2),
    ("format", 1),
    ("summar", 1),
    ("list the", 1),
    ("simple", 1),
    ("small", 1),
    ("reply with that one line", 1),
];

/// How hard `task` is. An explicit `difficulty: easy|medium|hard` line (or `[hard]`) in the text
/// decides; otherwise the signals in the text are added up.
pub fn decide(task: &str) -> Decision {
    let lower = task.to_lowercase();
    for line in lower.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '#', ' ']);
        let explicit = line
            .strip_prefix("difficulty:")
            .or_else(|| line.strip_prefix("difficulty ="))
            .map(str::trim);
        for (word, difficulty) in [
            ("easy", Difficulty::Easy),
            ("medium", Difficulty::Medium),
            ("hard", Difficulty::Hard),
        ] {
            if explicit == Some(word) || line == format!("[{word}]") {
                return Decision {
                    difficulty,
                    reason: "the task says so".to_string(),
                };
            }
        }
    }

    let mut score = 0;
    let mut reasons: Vec<String> = Vec::new();
    let length = task.chars().count();
    match length {
        0..=240 => {
            score -= 1;
            reasons.push("short".to_string());
        }
        241..=1_200 => {}
        1_201..=3_000 => {
            score += 1;
            reasons.push("long".to_string());
        }
        _ => {
            score += 2;
            reasons.push("very long".to_string());
        }
    }
    let paths = lower
        .split_whitespace()
        .filter(|word| {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.');
            word.contains('/') && word.chars().any(|c| c.is_alphabetic()) && word.len() > 3
        })
        .count();
    if paths >= 4 {
        score += 2;
        reasons.push(format!("names {paths} paths"));
    } else if paths >= 2 {
        score += 1;
        reasons.push(format!("names {paths} paths"));
    }
    if task.matches("```").count() >= 4 {
        score += 1;
        reasons.push("has code blocks".to_string());
    }
    for (word, weight) in HARD_WORDS {
        if lower.contains(word) {
            score += weight;
            reasons.push(format!("\"{word}\""));
        }
    }
    for (word, weight) in EASY_WORDS {
        if lower.contains(word) {
            score -= weight;
            reasons.push(format!("\"{word}\" (easier)"));
        }
    }

    let difficulty = match score {
        i32::MIN..=-2 => Difficulty::Easy,
        -1..=1 => Difficulty::Medium,
        _ => Difficulty::Hard,
    };
    let reason = if reasons.is_empty() {
        "nothing stands out".to_string()
    } else {
        reasons.join(", ")
    };
    Decision { difficulty, reason }
}

/// Words in a model's name that say it is strong or weak, with their weight. A name is cut at every
/// character that isn't a letter or digit and a word must match a whole piece (so "mini" is not in
/// "gemini"); several words joined by `-` match in a row ("gpt-5", "glm-4-7" for GLM-4.7); a
/// trailing `*` lets the last piece run on ("deepseek-v*" is deepseek-v3, deepseek-v4, ...).
const STRONG_NAMES: &[(&str, f64)] = &[
    ("opus", 3.0),
    ("gpt-5", 3.0),
    ("pro", 1.5),
    ("thinking", 1.0),
    ("reasoner", 1.0),
    ("large", 1.0),
    ("coder", 0.5),
    ("coding", 0.5),
    ("sonnet", 2.0),
    ("glm-4-7", 1.5),
    ("deepseek-v*", 1.5),
    ("kimi-k*", 2.0),
    ("qwen3-coder", 1.5),
    ("qwen3-6", 1.5),
    ("qwen3-5", 0.8),
    ("gpt-oss", 1.0),
    ("nemotron", 0.5),
];
const WEAK_NAMES: &[(&str, f64)] = &[
    ("nano", 2.0),
    ("mini", 1.5),
    ("lite", 1.5),
    ("flash", 1.0),
    ("small", 1.0),
    ("haiku", 1.0),
    ("tiny", 2.0),
    ("instant", 0.5),
];

/// A model name as `-piece-piece-`, cut at every character that isn't a letter or digit.
fn name_pieces(model: &str) -> String {
    let lower = model.to_lowercase();
    let pieces: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|piece| !piece.is_empty())
        .collect();
    format!("-{}-", pieces.join("-"))
}

/// Whether `pieces` (from [`name_pieces`]) contain `word` as described at [`STRONG_NAMES`].
fn has_word(pieces: &str, word: &str) -> bool {
    match word.strip_suffix('*') {
        Some(prefix) => pieces.contains(&format!("-{prefix}")),
        None => pieces.contains(&format!("-{word}-")),
    }
}

/// The size in billions of parameters a model's name states ("gpt-oss-120b", "qwen3.5:9b").
fn billions_in(name: &str) -> Option<f64> {
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<f64> = None;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'b' {
            continue;
        }
        // The number just before the "b", and the "b" not part of a longer word.
        if bytes.get(index + 1).is_some_and(|next| next.is_ascii_alphabetic()) {
            continue;
        }
        let start = bytes[..index]
            .iter()
            .rposition(|c| !(c.is_ascii_digit() || *c == b'.'))
            .map_or(0, |at| at + 1);
        if let Ok(value) = lower[start..index].parse::<f64>() {
            if value > 0.0 && best.is_none_or(|known| value > known) {
                best = Some(value);
            }
        }
    }
    best
}

/// How strong a model looks, from its name, price (USD per million input and output tokens) and
/// context window. Only the order matters: a higher number is a stronger model.
pub fn strength(model: &str, price: Option<(f64, f64)>, context_window: Option<u64>) -> f64 {
    let pieces = name_pieces(model);
    // A name that states its size is ranked by it (9B ~ 2.2, 30B ~ 3.4, 120B ~ 4.8, 400B ~ 6); a
    // cloud model that doesn't is a typical hosted one, about 55B.
    let mut score = billions_in(model).map_or(4.0, f64::ln);
    for (word, weight) in STRONG_NAMES {
        if has_word(&pieces, word) {
            score += weight;
        }
    }
    for (word, weight) in WEAK_NAMES {
        if has_word(&pieces, word) {
            score -= weight;
        }
    }
    if let Some((input, output)) = price {
        // A paid model is usually the stronger of its provider's; free says nothing.
        score += (1.0 + input + output).ln() * 0.5;
    }
    if let Some(window) = context_window {
        score += (window as f64 / 8_192.0).max(1.0).ln() * 0.1;
    }
    score
}

/// Which of `strengths` (one per available model) a task of this difficulty goes to: the index of
/// the one whose rank among them is nearest the difficulty's target, the strongest for hard and the
/// weakest for easy; of equal strength, hard takes the earlier (the failover list's order). `None`
/// when empty.
pub fn pick_rank(difficulty: Difficulty, strengths: &[f64]) -> Option<usize> {
    if strengths.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..strengths.len()).collect();
    order.sort_by(|a, b| {
        strengths[*a]
            .partial_cmp(&strengths[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.cmp(a))
    });
    let last = (order.len() - 1).max(1) as f64;
    order
        .iter()
        .enumerate()
        .min_by(|(rank_a, _), (rank_b, _)| {
            let distance = |rank: &usize| (*rank as f64 / last - difficulty.target_rank()).abs();
            distance(rank_a)
                .partial_cmp(&distance(rank_b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, index)| *index)
}

/// What a pick optimises, the three "auto" flavours of the model picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The weakest model that fits the difficulty, free ones first for easy and medium work.
    CostEfficient,
    /// The fastest model that is strong enough for the difficulty.
    Responsive,
    /// The strongest model there is.
    Genius,
}

impl Policy {
    pub fn label(self) -> &'static str {
        match self {
            Policy::CostEfficient => "cost-efficient",
            Policy::Responsive => "responsive",
            Policy::Genius => "genius",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        [Policy::CostEfficient, Policy::Responsive, Policy::Genius]
            .into_iter()
            .find(|policy| policy.label() == label)
    }
}

/// One model a task could go to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    /// [`strength`] of the model.
    pub strength: f64,
    /// [`speed`] of the model.
    pub speed: f64,
    /// Whether it costs nothing.
    pub free: bool,
}

/// How fast a model answers, from what its name says about the parameters it uses per token: a
/// mixture-of-experts name states them ("a3b" is 3B active), a dense one is its size, and a
/// hosted model that states neither is taken as mid-sized. Higher is faster.
pub fn speed(model: &str) -> f64 {
    let lower = model.to_lowercase();
    let active = lower
        .match_indices('a')
        .filter_map(|(index, _)| {
            let rest = &lower[index + 1..];
            let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            (end > 0 && rest[end..].starts_with('b'))
                .then(|| rest[..end].parse::<f64>().ok())
                .flatten()
        })
        .next()
        .or_else(|| billions_in(model))
        .unwrap_or(20.0);
    let pieces = name_pieces(model);
    let quick = ["flash", "mini", "nano", "lite", "instant", "small", "haiku"]
        .iter()
        .any(|word| has_word(&pieces, word));
    1.0 / (1.0 + if quick { active * 0.3 } else { active })
}

/// Index of the candidate `policy` sends a task of this `difficulty` to, or `None` with no
/// candidates. Of equal candidates the earlier wins, so the failover list's order is the tiebreak.
pub fn choose(policy: Policy, difficulty: Difficulty, candidates: &[Candidate]) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    let by_strength: Vec<f64> = candidates.iter().map(|candidate| candidate.strength).collect();
    match policy {
        Policy::Genius => (0..candidates.len()).fold(0, |best, index| {
            if candidates[index].strength > candidates[best].strength {
                index
            } else {
                best
            }
        })
        .into(),
        Policy::CostEfficient => {
            // Free models first for work that is not hard, when there are any.
            let pool: Vec<usize> = if difficulty != Difficulty::Hard
                && candidates.iter().any(|candidate| candidate.free)
            {
                (0..candidates.len()).filter(|&i| candidates[i].free).collect()
            } else {
                (0..candidates.len()).collect()
            };
            let strengths: Vec<f64> = pool.iter().map(|&i| by_strength[i]).collect();
            pick_rank(difficulty, &strengths).map(|at| pool[at])
        }
        Policy::Responsive => {
            // Strong enough: in the top part of the ranking the difficulty asks for.
            let floor = match difficulty {
                Difficulty::Easy => 0.0,
                Difficulty::Medium => 0.34,
                Difficulty::Hard => 0.67,
            };
            let last = (candidates.len() - 1).max(1) as f64;
            let mut order: Vec<usize> = (0..candidates.len()).collect();
            order.sort_by(|a, b| {
                by_strength[*a]
                    .partial_cmp(&by_strength[*b])
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.cmp(a))
            });
            let mut best: Option<usize> = None;
            for (rank, &index) in order.iter().enumerate() {
                if (rank as f64) / last < floor {
                    continue;
                }
                best = match best {
                    Some(known)
                        if candidates[known].speed > candidates[index].speed
                            || (candidates[known].speed == candidates[index].speed
                                && by_strength[known] >= by_strength[index]) =>
                    {
                        Some(known)
                    }
                    _ => Some(index),
                };
            }
            // A pool too small to have anything in the top part: its strongest.
            best.or_else(|| order.last().copied())
        }
    }
}
