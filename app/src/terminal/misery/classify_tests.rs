use super::*;

fn parsed(input: &str) -> ParsedCommand {
    parse(input).expect("should parse")
}

#[test]
fn classifies_common_commands() {
    assert_eq!(parsed("ls").class, CommandClass::Listing);
    assert_eq!(parsed("ls -la").class, CommandClass::Listing);
    assert_eq!(parsed("cd ..").class, CommandClass::Navigation);
    assert_eq!(parsed("rm -rf build").class, CommandClass::Destructive);
    assert_eq!(parsed("clear").class, CommandClass::Clear);
    assert_eq!(parsed("git status").class, CommandClass::Vcs);
    assert_eq!(
        parsed("curl https://example.com").class,
        CommandClass::Network
    );
}

#[test]
fn separates_flags_from_positionals() {
    let cmd = parsed("ls -la --color=auto src");
    assert_eq!(cmd.name, "ls");
    assert_eq!(cmd.flags, vec!["la", "color=auto"]);
    assert_eq!(cmd.args, vec!["src"]);
}

#[test]
fn negative_numbers_are_not_flags() {
    // `tail -5` reads as a flag, but `-5` alone shouldn't be treated as one.
    let cmd = parsed("some_tool -5");
    assert!(cmd.flags.is_empty(), "flags: {:?}", cmd.flags);
    assert_eq!(cmd.args, vec!["-5"]);
}

#[test]
fn subcommands_only_for_commands_that_have_them() {
    let git = parsed("git commit -m wip");
    assert_eq!(git.subcommand.as_deref(), Some("commit"));

    // `cat` has no subcommands; the filename must stay a positional.
    let cat = parsed("cat notes.txt");
    assert_eq!(cat.subcommand, None);
    assert_eq!(cat.args, vec!["notes.txt"]);
}

#[test]
fn package_management_distinguished_from_build() {
    assert_eq!(parsed("npm install").class, CommandClass::PackageMgmt);
    assert_eq!(parsed("npm run build").class, CommandClass::Build);
    assert_eq!(parsed("cargo add serde").class, CommandClass::PackageMgmt);
    assert_eq!(parsed("cargo build").class, CommandClass::Build);
}

#[test]
fn strips_wrappers_and_env_assignments() {
    assert_eq!(parsed("sudo rm -rf /tmp/x").name, "rm");
    assert_eq!(parsed("RUST_LOG=debug cargo test").name, "cargo");
    assert_eq!(parsed("env FOO=1 ls").name, "ls");
}

#[test]
fn strips_paths_and_exe_suffix() {
    assert_eq!(parsed("/usr/bin/ls").name, "ls");
    assert_eq!(parsed("C:\\tools\\grep.exe foo").name, "grep");
}

#[test]
fn detects_compound_commands() {
    assert!(parsed("ls | wc -l").is_compound);
    assert!(parsed("make && ./run").is_compound);
    assert!(parsed("echo hi > out.txt").is_compound);
    assert!(!parsed("ls -la").is_compound);
}

#[test]
fn compound_classifies_on_first_segment() {
    // The intent is the listing; `wc` is just plumbing.
    assert_eq!(parsed("ls | wc -l").class, CommandClass::Listing);
}

#[test]
fn bare_detection() {
    assert!(parsed("ls").is_bare());
    assert!(!parsed("ls -la").is_bare());
    assert!(!parsed("git status").is_bare());
}

#[test]
fn empty_input_yields_nothing() {
    assert!(parse("").is_none());
    assert!(parse("   ").is_none());
}

#[test]
fn unknown_commands_do_not_panic() {
    let cmd = parsed("frobnicate --wildly");
    assert_eq!(cmd.class, CommandClass::Unknown);
}
