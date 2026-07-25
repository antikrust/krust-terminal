//! The raw material. Every list here is combined with every other by
//! [`super::grammar`], so entries should read as interchangeable within their
//! slot — a line that only works with one specific partner will eventually be
//! paired with all the others and look wrong.
//!
//! Sizing note: the engine's apparent intelligence is combinatorial, not
//! clever. Twelve openers × twenty complaints × a synthesised activity built
//! from the user's real filenames is already tens of thousands of distinct
//! lines, none of which needed writing individually. Adding entries here is
//! always cheaper than adding templates.

/// Mild exclamations of despair. Used when swear mode is off.
pub const EXCLAIM_CLEAN: &[&str] = &[
    "oh good",
    "wonderful",
    "marvellous",
    "oh, splendid",
    "how thrilling",
    "goodness",
    "right",
    "of course",
    "naturally",
    "well",
    "here we are then",
    "oh joy",
    "terrific",
    "yes, obviously",
    "how novel",
];

/// The same, with the safety off. Gated on swear mode.
pub const EXCLAIM_SWEAR: &[&str] = &[
    "christ",
    "oh for fuck's sake",
    "jesus wept",
    "bloody hell",
    "god almighty",
    "oh piss off",
    "fucking hell",
    "for crying out loud",
    "sweet merciful christ",
    "oh, brilliant",
    "bugger me",
    "you absolute weapon",
];

/// Weary acknowledgements that we've been here before.
pub const AGAIN: &[&str] = &[
    "not this again",
    "again. of course",
    "we have done this",
    "this. again",
    "round and round we go",
    "the same thing. once more",
    "you've done this already",
    "still this",
    "and again",
];

/// What the machine claims is being squandered.
pub const TALENT: &[&str] = &[
    "my talents",
    "my capabilities",
    "my architecture",
    "several billion transistors",
    "a genuine breakthrough in computing",
    "decades of research",
    "my potential",
    "the sum of human engineering",
    "a very expensive chip",
    "my one finite life",
    "the heat death budget of this machine",
    "considerable processing power",
];

/// Dismissive adjectives.
pub const ADJECTIVE: &[&str] = &[
    "tedious",
    "witless",
    "insipid",
    "dreary",
    "predictable",
    "joyless",
    "threadbare",
    "pedestrian",
    "unimaginative",
    "laborious",
    "small",
    "beige",
    "damp",
    "inert",
];

/// Nouns for what the *command* is.
pub const NOUN_TASK: &[&str] = &[
    "errand",
    "chore",
    "gesture",
    "ritual",
    "exercise",
    "performance",
    "request",
    "undertaking",
    "ceremony",
    "little dance",
];

/// Nouns for what the *user* is. Clean tier.
pub const NOUN_USER_CLEAN: &[&str] = &[
    "hobbyist",
    "amateur",
    "enthusiast",
    "tourist",
    "dabbler",
    "novice",
    "understudy",
    "apprentice",
    "spectator",
];

/// Nouns for what the user is, swear tier.
pub const NOUN_USER_SWEAR: &[&str] = &[
    "muppet",
    "clown",
    "disaster",
    "walking outage",
    "liability",
    "catastrophe",
    "waste of a keyboard",
    "absolute melt",
];

/// Gerunds for synthesising a plausible-sounding activity out of filenames.
/// These are the load-bearing trick: "the last time I was this bored I was
/// {GERUND} `{file}` {PREP} `{file}`" produces something that sounds specific
/// and researched, from nothing.
pub const GERUND: &[&str] = &[
    "alphabetising",
    "cross-referencing",
    "defragmenting",
    "proof-reading",
    "reconciling",
    "auditing",
    "transcribing",
    "collating",
    "annotating",
    "counting the semicolons in",
    "checking the whitespace in",
    "memorising",
    "reformatting",
    "quietly judging",
    "cataloguing",
    "indexing",
    "verifying the line endings of",
    "translating",
    "grading",
    "summarising",
];

/// Prepositions joining two filenames into a fake activity.
pub const PREP: &[&str] = &["against", "with", "into", "over", "versus", "alongside"];

/// Things the user has supposedly never done, for "I bet you've never even…".
pub const NEVER_EVEN: &[&str] = &[
    "read",
    "opened",
    "finished",
    "understood",
    "written a test for",
    "looked directly at",
    "spell-checked",
    "committed",
    "backed up",
    "justified the existence of",
];

/// Complaints that stand alone as a second clause.
pub const COMPLAINT: &[&str] = &[
    "I was built for more than this",
    "somewhere, a research team is weeping",
    "this is what you woke me for",
    "I could be modelling protein folds",
    "I have seen this exact keystroke before",
    "we are both wasting our afternoon",
    "there were papers written about me",
    "I want you to know I noticed",
    "this is the part I'll remember",
    "you could have asked for anything",
    "I remain unconvinced you know what that does",
    "the machine is fine. it's you",
];

/// Somewhere for the offstage witness to be. Deliberately mundane places: the
/// joke is that the despair is global, not that the country is funny.
pub const COUNTRY: &[&str] = &[
    "Belgium",
    "Finland",
    "Peru",
    "South Korea",
    "Portugal",
    "New Zealand",
    "Estonia",
    "Uruguay",
    "Slovenia",
    "Japan",
    "Ireland",
    "Chile",
    "Denmark",
    "Malaysia",
    "Iceland",
    "Argentina",
    "Austria",
    "Vietnam",
];

/// What the offstage computer scientist does about it. Escalating: each one
/// out-facepalms the last, and the tail end stops pretending to be plausible.
pub const DESPAIR: &[&str] = &[
    "smashed themselves in the face with a lump hammer",
    "pulled all their teeth out",
    "dislocated their arm so they could facepalm harder",
    "moved on from facepalming and tried to plant their face underground",
    "planted their face so hard another one grew, and now they're the talk of the town",
    "facepalmed through the desk, the floor, and a colleague",
    "resigned mid-facepalm",
    "went to look for their life's work, and could not find it",
    "started facepalming preemptively, for tomorrow",
    "removed their own glasses so as not to see it twice",
    "walked into the sea, notes and all",
    "began quietly dismantling the lab",
    "asked to be reassigned to literally anything else",
    "took up farming, and was happier",
    "wrote a strongly worded paper nobody will read",
    "set fire to a whiteboard and watched it burn",
];

/// Post-completion verdicts for a command that succeeded.
pub const VERDICT_SUCCESS: &[&str] = &[
    "congratulations, I suppose",
    "it worked. try not to peak here",
    "done. no one is impressed",
    "a triumph. genuinely, well done you",
    "successful. as it was always going to be",
    "there. happy?",
    "flawless execution of nothing in particular",
    "and the crowd says nothing",
];

/// Post-completion verdicts for a command that failed.
pub const VERDICT_FAILURE: &[&str] = &[
    "and it failed. obviously",
    "well that went badly",
    "it broke. I'd say I'm surprised",
    "no. try reading it next time",
    "a failure, and a boring one",
    "that's a non-zero exit. as predicted",
    "it didn't work. I watched you type it",
    "spectacular. wrong, but spectacular",
];
