//! Classifying a command into the categories misery mode cares about.
//!
//! The point of classification is *topicality*: a jab about wasted talent
//! should land on `ls`, not on a 40-minute build. Everything here works off the
//! command string alone so the engine stays testable without a live shell; the
//! caller can pass a name already extracted by the real parser
//! (`derp_completer::parsers::hir::Command::command_name_span`) when it has
//! one, which is more accurate for pipelines and assignments.

/// What kind of thing the user just asked for. Drives both template selection
/// and how deserving of contempt the command is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandClass {
    /// `ls`, `ll`, `tree` — the archetypal waste of a supercomputer.
    Listing,
    /// `cd`, `pwd`, `pushd` — moving without doing.
    Navigation,
    /// `cat`, `less`, `head` — looking at things.
    Inspection,
    /// `grep`, `rg`, `find` — looking *for* things.
    Search,
    /// `git`, `jj`, `hg`.
    Vcs,
    /// `cargo`, `make`, `npm run`, `tsc`.
    Build,
    /// `brew`, `apt`, `pip`, `npm install`.
    PackageMgmt,
    /// `rm`, `dd`, `kill -9`, `mkfs`. Handled with a certain relish.
    Destructive,
    /// `vim`, `nano`, `code`.
    Editor,
    /// `curl`, `ssh`, `ping`, `wget`.
    Network,
    /// `ps`, `top`, `kill`.
    Process,
    /// `echo`, `export`, `alias`, `source`.
    Shell,
    /// `clear`, `reset` — the tell of someone who has lost control.
    Clear,
    /// Anything unrecognised.
    Unknown,
}

impl CommandClass {
    /// How intrinsically contemptible this class is, as a starting
    /// probability. Feeds the adaptive frequency in [`super::deserve`].
    ///
    /// These sit well below the clamp ceiling on purpose: the modifiers in
    /// `deserve` (repetition, failure, pipelines) need headroom to actually
    /// move the number. A baseline near the ceiling saturates and makes every
    /// adjustment a no-op.
    pub fn baseline_contempt(self) -> f32 {
        match self {
            CommandClass::Clear => 0.50,
            CommandClass::Listing => 0.45,
            CommandClass::Navigation => 0.42,
            CommandClass::Inspection => 0.30,
            CommandClass::Shell => 0.28,
            CommandClass::Process => 0.25,
            // Destructive commands earn attention, not contempt.
            CommandClass::Destructive => 0.25,
            CommandClass::Search => 0.20,
            CommandClass::Vcs => 0.18,
            CommandClass::Editor => 0.15,
            CommandClass::Network => 0.15,
            CommandClass::Unknown => 0.15,
            CommandClass::PackageMgmt => 0.12,
            // Building is the one thing that looks like work.
            CommandClass::Build => 0.07,
        }
    }
}

/// Everything the engine knows about the command that was typed.
#[derive(Debug, Clone)]
pub struct ParsedCommand {
    /// The bare executable name, lowercased, without path (`ls`, `git`).
    pub name: String,
    /// The subcommand, if the first positional looks like one (`git status`
    /// -> `status`). Used to separate `npm install` from `npm run build`.
    pub subcommand: Option<String>,
    /// Positional arguments, excluding the subcommand.
    pub args: Vec<String>,
    /// Flags, with leading dashes stripped (`-la` -> `la`, `--all` -> `all`).
    pub flags: Vec<String>,
    pub class: CommandClass,
    /// Whether the raw input contained a pipe, `&&`, `;` or a redirect. A
    /// pipeline is at least *trying*, which the engine grudgingly notes.
    pub is_compound: bool,
}

impl ParsedCommand {
    /// True if the command carries no arguments or flags at all — the purest
    /// form of doing nothing.
    pub fn is_bare(&self) -> bool {
        self.subcommand.is_none() && self.args.is_empty() && self.flags.is_empty()
    }
}

/// Parses a raw command line well enough for the purposes of insulting it.
///
/// This is deliberately not a shell parser. It takes the first word of the
/// first segment, splits flags from positionals, and gives up gracefully on
/// anything exotic — a wrong classification costs a slightly off-topic insult,
/// which is an acceptable failure mode.
pub fn parse(input: &str) -> Option<ParsedCommand> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let is_compound = trimmed.contains('|')
        || trimmed.contains("&&")
        || trimmed.contains(';')
        || trimmed.contains('>')
        || trimmed.contains('<');

    // Take the first segment of any pipeline/list — that's the command whose
    // intent we're judging.
    let first_segment = trimmed
        .split(&['|', ';'][..])
        .next()
        .unwrap_or(trimmed)
        .split("&&")
        .next()
        .unwrap_or(trimmed)
        .trim();

    let mut tokens = first_segment.split_whitespace().peekable();

    // Skip leading environment assignments (`FOO=bar cmd`) and `sudo`/`env`,
    // which are wrappers rather than the actual intent.
    let mut name_token = tokens.next()?;
    loop {
        let is_env_assignment = name_token.contains('=') && !name_token.starts_with('-');
        let is_wrapper = matches!(
            strip_path(name_token).as_str(),
            "sudo" | "env" | "time" | "nohup" | "command" | "doas"
        );
        if is_env_assignment || is_wrapper {
            match tokens.next() {
                Some(next) => name_token = next,
                None => break,
            }
        } else {
            break;
        }
    }

    let name = strip_path(name_token);
    if name.is_empty() {
        return None;
    }

    let mut args = Vec::new();
    let mut flags = Vec::new();
    for token in tokens {
        if let Some(long) = token.strip_prefix("--") {
            if !long.is_empty() {
                flags.push(long.to_owned());
            }
        } else if let Some(short) = token.strip_prefix('-') {
            // `-5` and a bare `-` are operands, not flags — keep them as
            // positionals rather than dropping them on the floor.
            let looks_numeric = short.chars().next().is_some_and(|c| c.is_ascii_digit());
            if short.is_empty() || looks_numeric {
                args.push(token.to_owned());
            } else {
                flags.push(short.to_owned());
            }
        } else {
            args.push(token.to_owned());
        }
    }

    // A subcommand is the first positional, but only for commands that
    // actually have subcommands — otherwise `cat foo.txt` would read as the
    // `foo.txt` subcommand.
    let takes_subcommand = matches!(
        name.as_str(),
        "git"
            | "cargo"
            | "npm"
            | "yarn"
            | "pnpm"
            | "bun"
            | "go"
            | "docker"
            | "kubectl"
            | "brew"
            | "apt"
            | "pip"
            | "pip3"
            | "jj"
            | "hg"
            | "systemctl"
            | "gh"
            | "terraform"
            | "aws"
    );
    let subcommand = if takes_subcommand && !args.is_empty() {
        Some(args.remove(0))
    } else {
        None
    };

    let class = classify(&name, subcommand.as_deref());

    Some(ParsedCommand {
        name,
        subcommand,
        args,
        flags,
        class,
        is_compound,
    })
}

/// Strips any directory prefix and a `.exe` suffix, and lowercases.
fn strip_path(token: &str) -> String {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .trim_end_matches(".exe");
    base.to_ascii_lowercase()
}

fn classify(name: &str, subcommand: Option<&str>) -> CommandClass {
    // Package management hides inside build tools; check the subcommand first.
    if let Some(sub) = subcommand {
        match (name, sub) {
            (
                "npm" | "yarn" | "pnpm" | "bun" | "pip" | "pip3",
                "install" | "i" | "add" | "remove",
            )
            | ("cargo", "install" | "add" | "remove")
            | ("go", "get" | "install")
            | ("brew", _)
            | ("apt", _) => return CommandClass::PackageMgmt,
            ("cargo", "build" | "run" | "test" | "check" | "clippy" | "bench")
            | ("npm" | "yarn" | "pnpm" | "bun", "run" | "build" | "test" | "start")
            | ("go", "build" | "run" | "test") => return CommandClass::Build,
            _ => {}
        }
    }

    match name {
        "ls" | "ll" | "la" | "l" | "dir" | "tree" | "exa" | "eza" | "lsd" => CommandClass::Listing,
        "cd" | "pwd" | "pushd" | "popd" | "dirs" | "z" | "j" => CommandClass::Navigation,
        "cat" | "less" | "more" | "head" | "tail" | "bat" | "wc" | "file" | "stat" | "du"
        | "df" | "open" => CommandClass::Inspection,
        "grep" | "rg" | "ag" | "ack" | "find" | "fd" | "fzf" | "locate" | "which" | "whereis" => {
            CommandClass::Search
        }
        "git" | "jj" | "hg" | "svn" | "gh" | "glab" => CommandClass::Vcs,
        "cargo" | "make" | "cmake" | "npm" | "yarn" | "pnpm" | "bun" | "go" | "tsc" | "gcc"
        | "clang" | "rustc" | "javac" | "gradle" | "mvn" | "bazel" | "ninja" => CommandClass::Build,
        "brew" | "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "pip" | "pip3" | "gem" | "port"
        | "snap" | "nix" => CommandClass::PackageMgmt,
        "rm" | "rmdir" | "dd" | "mkfs" | "shred" | "truncate" | "unlink" => {
            CommandClass::Destructive
        }
        "vim" | "nvim" | "vi" | "nano" | "emacs" | "code" | "subl" | "hx" | "micro" | "pico" => {
            CommandClass::Editor
        }
        "curl" | "wget" | "ssh" | "scp" | "rsync" | "ping" | "dig" | "nc" | "telnet"
        | "traceroute" | "http" | "httpie" => CommandClass::Network,
        "ps" | "top" | "htop" | "btop" | "kill" | "killall" | "pkill" | "jobs" | "bg" | "fg"
        | "lsof" => CommandClass::Process,
        "echo" | "export" | "alias" | "unalias" | "source" | "set" | "unset" | "printf"
        | "history" | "type" => CommandClass::Shell,
        "clear" | "reset" | "cls" => CommandClass::Clear,
        _ => CommandClass::Unknown,
    }
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
