use std::collections::HashSet;

use clap_complete::aot::Shell;
use local_control::protocol::{ActionKind, ControlError, ErrorCode};
use serde_json::json;

use super::*;

#[test]
fn parses_typed_create_and_setting_list_params() {
    let args = ControlArgs::try_parse_from([
        "borkctrl",
        "tab",
        "create",
        "--type",
        "agent",
        "--shell",
        "zsh",
        "--session",
        "session_1",
    ])
    .expect("tab create parses");
    let ControlCommand::Tab(TabCommand::Create(args)) = args.command else {
        panic!("expected tab create command");
    };
    assert_eq!(args.tab_type, Some(CliTabType::Agent));
    assert_eq!(args.shell.as_deref(), Some("zsh"));
    assert_eq!(args.target.session.as_deref(), Some("session_1"));

    let args =
        ControlArgs::try_parse_from(["borkctrl", "setting", "list", "--namespace", "editor"])
            .expect("setting list parses");
    let ControlCommand::Setting(SettingCommand::List(args)) = args.command else {
        panic!("expected setting list command");
    };
    assert_eq!(args.namespace.as_deref(), Some("editor"));
}

#[test]
fn rejects_conflicting_instance_selectors() {
    let err = ControlArgs::try_parse_from([
        "borkctrl",
        "tab",
        "create",
        "--instance",
        "inst_123",
        "--pid",
        "123",
    ])
    .expect_err("instance and pid conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn parses_instance_and_pid_selectors() {
    let args = ControlArgs::try_parse_from(["borkctrl", "tab", "create", "--instance", "inst_123"])
        .expect("instance selector parses");
    let ControlCommand::Tab(TabCommand::Create(create)) = args.command else {
        panic!("expected tab create command");
    };
    assert_eq!(create.target.instance.as_deref(), Some("inst_123"));

    let args = ControlArgs::try_parse_from(["borkctrl", "app", "ping", "--pid", "123"])
        .expect("pid selector parses");
    let ControlCommand::App(AppCommand::Ping(target)) = args.command else {
        panic!("expected app ping command");
    };
    assert_eq!(target.pid, Some(123));
}

#[test]
fn surface_list_accepts_instance_selection() {
    let args =
        ControlArgs::try_parse_from(["borkctrl", "surface", "list", "--instance", "inst_123"])
            .expect("surface list instance selector parses");
    let ControlCommand::Surface(SurfaceCommand::List(target)) = args.command else {
        panic!("expected surface list command");
    };
    assert_eq!(target.instance.as_deref(), Some("inst_123"));
}

#[test]
fn rejects_excluded_command_routes() {
    for args in [
        vec!["borkctrl", "history", "list"],
        vec!["borkctrl", "block", "list"],
        vec!["borkctrl", "block", "inspect", "block_1"],
        vec!["borkctrl", "block", "output", "block_1"],
        vec!["borkctrl", "input", "get"],
        vec!["borkctrl", "input", "clear"],
        vec!["borkctrl", "input", "mode", "set", "agent"],
        vec!["borkctrl", "input", "run", "pwd"],
        vec!["borkctrl", "file", "list"],
        vec!["borkctrl", "drive", "list"],
        vec!["borkctrl", "auth", "status"],
    ] {
        assert!(ControlArgs::try_parse_from(args).is_err());
    }
}

#[test]
fn parses_first_slice_instance_list() {
    let args = ControlArgs::try_parse_from(["borkctrl", "instance", "list"])
        .expect("instance list parses");
    assert!(matches!(
        args.command,
        ControlCommand::Instance(InstanceCommand::List)
    ));
}

#[test]
fn parses_first_slice_app_smoke_metadata_commands() {
    assert!(ControlArgs::try_parse_from(["borkctrl", "app", "ping"]).is_ok());
    assert!(ControlArgs::try_parse_from(["borkctrl", "app", "version"]).is_ok());
    assert!(ControlArgs::try_parse_from(["borkctrl", "app", "active"]).is_ok());
    assert!(ControlArgs::try_parse_from(["borkctrl", "app", "focus"]).is_ok());
}

#[test]
fn parses_catalog_metadata_commands() {
    let args =
        ControlArgs::try_parse_from(["borkctrl", "action", "inspect", "surface.settings.open"])
            .expect("action inspect parses");
    let ControlCommand::Action(ActionCatalogCommand::Inspect { action }) = args.command else {
        panic!("expected action inspect command");
    };
    assert_eq!(action, "surface.settings.open");
    assert!(ControlArgs::try_parse_from(["borkctrl", "action", "list"]).is_ok());
    assert!(ControlArgs::try_parse_from(["borkctrl", "capability", "list"]).is_ok());
    assert!(
        ControlArgs::try_parse_from(["borkctrl", "capability", "inspect", "tab.create"]).is_ok()
    );
}

#[test]
fn parses_control_mode_args_after_hidden_flag() {
    let args = ControlArgs::try_parse_control_mode_from(["bork", "--borkctrl", "tab", "create"])
        .expect("control mode flag is present")
        .expect("control mode args parse");
    assert!(matches!(
        args.command,
        ControlCommand::Tab(TabCommand::Create(_))
    ));
}

#[test]
fn ignores_args_without_control_mode_flag() {
    assert!(ControlArgs::try_parse_control_mode_from(["bork", "tab", "create"]).is_none());
}

#[test]
fn parses_completion_generation_command() {
    let args = ControlArgs::try_parse_from(["borkctrl", "completions", "bash"])
        .expect("completions parses");
    assert!(matches!(
        args.command,
        ControlCommand::Completions {
            shell: Some(Shell::Bash)
        }
    ));
}

#[test]
fn parses_exact_window_tab_pane_and_session_selectors() {
    let args = ControlArgs::try_parse_from([
        "borkctrl",
        "session",
        "inspect",
        "--window-title",
        "docs",
        "--tab-index",
        "2",
        "--pane",
        "pane_1",
        "--session",
        "session_1",
    ])
    .expect("exact target selectors parse");
    let ControlCommand::Session(SessionCommand::Inspect(target)) = args.command else {
        panic!("expected session inspect command");
    };
    assert_eq!(target.window_title.as_deref(), Some("docs"));
    assert_eq!(target.tab_index, Some(2));
    assert_eq!(target.pane.as_deref(), Some("pane_1"));
    assert_eq!(target.session.as_deref(), Some("session_1"));
}

#[test]
fn instance_list_output_serializes_empty_and_populated_lists() {
    let empty = serde_json::to_value(commands::instance_list_output(Vec::new()))
        .expect("empty list serializes");
    assert_eq!(empty, json!({ "instances": [] }));

    let record = local_control::discovery::InstanceRecord::for_current_process(
        None,
        "dev",
        "bork.baby.Bork",
        Some("v0.1.0".to_owned()),
        Vec::new(),
    );
    let instance_id = record.instance_id.0.clone();
    let populated = serde_json::to_value(commands::instance_list_output(vec![record]))
        .expect("populated list serializes");
    assert_eq!(populated["instances"][0]["instance_id"], json!(instance_id));
    assert_eq!(populated["instances"][0]["channel"], json!("dev"));
    assert_eq!(populated["instances"][0]["app_id"], json!("bork.baby.Bork"));
    assert_eq!(populated["instances"][0]["app_version"], json!("v0.1.0"));
}

#[test]
fn excluded_actions_are_not_allowlisted_catalog_entries() {
    for excluded in ["auth.api_key.set", "file.write", "block.list"] {
        assert!(
            ActionKind::ALL
                .iter()
                .all(|action| action.as_str() != excluded)
        );
    }
}

#[test]
fn generated_bash_completions_include_readonly_commands() {
    let completions =
        generate_completion_string(Shell::Bash).expect("bash completions render to UTF-8");
    assert!(completions.contains("instance"));
    assert!(completions.contains("action"));
    assert!(completions.contains("capability"));
    assert!(!completions.contains("stubs-only"));
    assert!(completions.contains("window"));
    assert!(completions.contains("input"));
    assert!(completions.contains("completions"));
    assert!(!completions.contains("block"));
}

#[test]
fn every_retained_catalog_action_has_a_parseable_cli_example() {
    let mut covered = HashSet::new();
    for (kind, argv) in retained_action_examples() {
        let args = ControlArgs::try_parse_from(argv)
            .unwrap_or_else(|err| panic!("{} parses: {err}", kind.as_str()));
        assert_eq!(parsed_action_kind(&args.command), Some(kind));
        covered.insert(kind);
    }
    let expected = ActionKind::ALL.iter().copied().collect::<HashSet<_>>();
    let missing = expected
        .difference(&covered)
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "retained catalog actions missing parser examples: {missing:?}"
    );
}

#[test]
fn generated_bash_completions_include_mutating_command_groups() {
    let completions =
        generate_completion_string(Shell::Bash).expect("bash completions render to UTF-8");
    assert!(completions.contains("surface"));
    assert!(completions.contains("command-palette"));
    assert!(completions.contains("bork-drive"));
    assert!(completions.contains("resource-center"));
    assert!(completions.contains("activate"));
    assert!(completions.contains("split"));
    assert!(!completions.contains("history"));
    assert!(!completions.contains("share-to-team"));
}

#[test]
fn structured_error_output_uses_stable_error_code() {
    let error = ControlError::new(ErrorCode::NoInstance, "no local Bork control instances");
    let value = serde_json::to_value(ErrorSummary {
        ok: false,
        error: &error,
    })
    .expect("error summary serializes");
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["error"]["code"], json!("no_instance"));
    assert_eq!(
        value["error"]["message"],
        json!("no local Bork control instances")
    );
}

#[test]
fn renders_human_readable_tab_create_output() {
    let rendered = render_human_readable_for_test(
        local_control::protocol::ActionKind::TabCreate,
        &json!({
            "tab": {
                "id": "tab_123",
                "active_index": 2,
                "count": 3
            },
            "window": {
                "id": "window_123"
            }
        }),
    );
    assert_eq!(
        rendered,
        "Created tab tab_123 in window window_123 (active index 2, tab count 3)"
    );
}

fn retained_action_examples() -> Vec<(ActionKind, Vec<&'static str>)> {
    vec![
        (
            ActionKind::InstanceList,
            vec!["borkctrl", "instance", "list"],
        ),
        (
            ActionKind::InstanceInspect,
            vec!["borkctrl", "instance", "inspect"],
        ),
        (ActionKind::AppPing, vec!["borkctrl", "app", "ping"]),
        (ActionKind::AppVersion, vec!["borkctrl", "app", "version"]),
        (ActionKind::AppActive, vec!["borkctrl", "app", "active"]),
        (ActionKind::AppFocus, vec!["borkctrl", "app", "focus"]),
        (
            ActionKind::CapabilityList,
            vec!["borkctrl", "capability", "list"],
        ),
        (
            ActionKind::CapabilityInspect,
            vec!["borkctrl", "capability", "inspect", "tab.create"],
        ),
        (ActionKind::WindowList, vec!["borkctrl", "window", "list"]),
        (
            ActionKind::WindowInspect,
            vec!["borkctrl", "window", "inspect"],
        ),
        (
            ActionKind::WindowCreate,
            vec!["borkctrl", "window", "create"],
        ),
        (ActionKind::WindowFocus, vec!["borkctrl", "window", "focus"]),
        (ActionKind::WindowClose, vec!["borkctrl", "window", "close"]),
        (ActionKind::TabList, vec!["borkctrl", "tab", "list"]),
        (ActionKind::TabInspect, vec!["borkctrl", "tab", "inspect"]),
        (ActionKind::TabCreate, vec!["borkctrl", "tab", "create"]),
        (ActionKind::TabActivate, vec!["borkctrl", "tab", "activate"]),
        (
            ActionKind::TabMove,
            vec!["borkctrl", "tab", "move", "--direction", "next"],
        ),
        (ActionKind::TabClose, vec!["borkctrl", "tab", "close"]),
        (
            ActionKind::TabRename,
            vec!["borkctrl", "tab", "rename", "docs"],
        ),
        (
            ActionKind::TabResetName,
            vec!["borkctrl", "tab", "reset-name"],
        ),
        (
            ActionKind::TabColorSet,
            vec!["borkctrl", "tab", "color", "set", "red"],
        ),
        (
            ActionKind::TabColorClear,
            vec!["borkctrl", "tab", "color", "clear"],
        ),
        (ActionKind::PaneList, vec!["borkctrl", "pane", "list"]),
        (ActionKind::PaneInspect, vec!["borkctrl", "pane", "inspect"]),
        (
            ActionKind::PaneSplit,
            vec!["borkctrl", "pane", "split", "--direction", "right"],
        ),
        (ActionKind::PaneFocus, vec!["borkctrl", "pane", "focus"]),
        (
            ActionKind::PaneNavigate,
            vec!["borkctrl", "pane", "navigate", "--direction", "next"],
        ),
        (
            ActionKind::PaneResize,
            vec![
                "borkctrl",
                "pane",
                "resize",
                "--direction",
                "right",
                "--amount",
                "4",
            ],
        ),
        (
            ActionKind::PaneMaximize,
            vec!["borkctrl", "pane", "maximize"],
        ),
        (
            ActionKind::PaneUnmaximize,
            vec!["borkctrl", "pane", "unmaximize"],
        ),
        (ActionKind::PaneClose, vec!["borkctrl", "pane", "close"]),
        (
            ActionKind::PaneRename,
            vec!["borkctrl", "pane", "rename", "server"],
        ),
        (
            ActionKind::PaneResetName,
            vec!["borkctrl", "pane", "reset-name"],
        ),
        (ActionKind::SessionList, vec!["borkctrl", "session", "list"]),
        (
            ActionKind::SessionInspect,
            vec!["borkctrl", "session", "inspect"],
        ),
        (
            ActionKind::SessionActivate,
            vec!["borkctrl", "session", "activate"],
        ),
        (
            ActionKind::SessionPrevious,
            vec!["borkctrl", "session", "previous"],
        ),
        (ActionKind::SessionNext, vec!["borkctrl", "session", "next"]),
        (
            ActionKind::SessionReopenClosed,
            vec!["borkctrl", "session", "reopen-closed"],
        ),
        (
            ActionKind::InputInsert,
            vec!["borkctrl", "input", "insert", "hello"],
        ),
        (
            ActionKind::InputReplace,
            vec!["borkctrl", "input", "replace", "hello"],
        ),
        (ActionKind::ThemeList, vec!["borkctrl", "theme", "list"]),
        (ActionKind::ThemeGet, vec!["borkctrl", "theme", "get"]),
        (
            ActionKind::ThemeSet,
            vec!["borkctrl", "theme", "set", "Dracula"],
        ),
        (
            ActionKind::ThemeSystemSet,
            vec!["borkctrl", "theme", "system-set", "true"],
        ),
        (
            ActionKind::ThemeLightSet,
            vec!["borkctrl", "theme", "light-set", "Light"],
        ),
        (
            ActionKind::ThemeDarkSet,
            vec!["borkctrl", "theme", "dark-set", "Dark"],
        ),
        (
            ActionKind::AppearanceGet,
            vec!["borkctrl", "appearance", "get"],
        ),
        (
            ActionKind::AppearanceFontSizeIncrease,
            vec!["borkctrl", "appearance", "font-size-increase"],
        ),
        (
            ActionKind::AppearanceFontSizeDecrease,
            vec!["borkctrl", "appearance", "font-size-decrease"],
        ),
        (
            ActionKind::AppearanceFontSizeReset,
            vec!["borkctrl", "appearance", "font-size-reset"],
        ),
        (
            ActionKind::AppearanceZoomIncrease,
            vec!["borkctrl", "appearance", "zoom-increase"],
        ),
        (
            ActionKind::AppearanceZoomDecrease,
            vec!["borkctrl", "appearance", "zoom-decrease"],
        ),
        (
            ActionKind::AppearanceZoomReset,
            vec!["borkctrl", "appearance", "zoom-reset"],
        ),
        (ActionKind::SettingList, vec!["borkctrl", "setting", "list"]),
        (
            ActionKind::SettingGet,
            vec!["borkctrl", "setting", "get", "font_size"],
        ),
        (
            ActionKind::SettingSet,
            vec!["borkctrl", "setting", "set", "font_size", "14"],
        ),
        (
            ActionKind::SettingToggle,
            vec!["borkctrl", "setting", "toggle", "autosuggestions"],
        ),
        (
            ActionKind::KeybindingList,
            vec!["borkctrl", "keybinding", "list"],
        ),
        (
            ActionKind::KeybindingGet,
            vec!["borkctrl", "keybinding", "get", "copy"],
        ),
        (ActionKind::ActionList, vec!["borkctrl", "action", "list"]),
        (
            ActionKind::ActionInspect,
            vec!["borkctrl", "action", "inspect", "tab.create"],
        ),
        (ActionKind::SurfaceList, vec!["borkctrl", "surface", "list"]),
        (
            ActionKind::SurfaceSettingsOpen,
            vec!["borkctrl", "surface", "settings", "open"],
        ),
        (
            ActionKind::SurfaceCommandPaletteOpen,
            vec!["borkctrl", "surface", "command-palette", "open"],
        ),
        (
            ActionKind::SurfaceCommandSearchOpen,
            vec!["borkctrl", "surface", "command-search", "open"],
        ),
        (
            ActionKind::SurfaceThemePickerOpen,
            vec!["borkctrl", "surface", "theme-picker", "open"],
        ),
        (
            ActionKind::SurfaceKeybindingsOpen,
            vec!["borkctrl", "surface", "keybindings", "open"],
        ),
        (
            ActionKind::SurfaceBorkDriveOpen,
            vec!["borkctrl", "surface", "bork-drive", "open"],
        ),
        (
            ActionKind::SurfaceBorkDriveToggle,
            vec!["borkctrl", "surface", "bork-drive", "toggle"],
        ),
        (
            ActionKind::SurfaceResourceCenterToggle,
            vec!["borkctrl", "surface", "resource-center", "toggle"],
        ),
        (
            ActionKind::SurfaceAiAssistantToggle,
            vec!["borkctrl", "surface", "ai-assistant", "toggle"],
        ),
        (
            ActionKind::SurfaceCodeReviewOpen,
            vec!["borkctrl", "surface", "code-review", "open"],
        ),
        (
            ActionKind::SurfaceCodeReviewToggle,
            vec!["borkctrl", "surface", "code-review", "toggle"],
        ),
        (
            ActionKind::SurfaceProjectExplorerOpen,
            vec!["borkctrl", "surface", "project-explorer", "open"],
        ),
        (
            ActionKind::SurfaceGlobalSearchOpen,
            vec!["borkctrl", "surface", "global-search", "open"],
        ),
        (
            ActionKind::SurfaceConversationListOpen,
            vec!["borkctrl", "surface", "conversation-list", "open"],
        ),
        (
            ActionKind::SurfaceLeftPanelToggle,
            vec!["borkctrl", "surface", "left-panel", "toggle"],
        ),
        (
            ActionKind::SurfaceRightPanelToggle,
            vec!["borkctrl", "surface", "right-panel", "toggle"],
        ),
        (
            ActionKind::SurfaceVerticalTabsOpen,
            vec!["borkctrl", "surface", "vertical-tabs", "open"],
        ),
        (
            ActionKind::SurfaceVerticalTabsToggle,
            vec!["borkctrl", "surface", "vertical-tabs", "toggle"],
        ),
        (
            ActionKind::SurfaceAgentManagementOpen,
            vec!["borkctrl", "surface", "agent-management", "open"],
        ),
        (
            ActionKind::FileOpen,
            vec!["borkctrl", "file", "open", "/tmp/example.txt"],
        ),
    ]
}

fn parsed_action_kind(command: &ControlCommand) -> Option<ActionKind> {
    match command {
        ControlCommand::Instance(command) => match command {
            InstanceCommand::List => Some(ActionKind::InstanceList),
            InstanceCommand::Inspect(_) => Some(ActionKind::InstanceInspect),
        },
        ControlCommand::App(command) => match command {
            AppCommand::Ping(_) => Some(ActionKind::AppPing),
            AppCommand::Version(_) => Some(ActionKind::AppVersion),
            AppCommand::Active(_) => Some(ActionKind::AppActive),
            AppCommand::Focus(_) => Some(ActionKind::AppFocus),
        },
        ControlCommand::Capability(command) => match command {
            CapabilityCommand::List => Some(ActionKind::CapabilityList),
            CapabilityCommand::Inspect { .. } => Some(ActionKind::CapabilityInspect),
        },
        ControlCommand::Action(command) => match command {
            ActionCatalogCommand::List => Some(ActionKind::ActionList),
            ActionCatalogCommand::Inspect { .. } => Some(ActionKind::ActionInspect),
        },
        ControlCommand::Window(command) => match command {
            WindowCommand::List(_) => Some(ActionKind::WindowList),
            WindowCommand::Inspect(_) => Some(ActionKind::WindowInspect),
            WindowCommand::Create(_) => Some(ActionKind::WindowCreate),
            WindowCommand::Focus(_) => Some(ActionKind::WindowFocus),
            WindowCommand::Close(_) => Some(ActionKind::WindowClose),
        },
        ControlCommand::Tab(command) => match command {
            TabCommand::List(_) => Some(ActionKind::TabList),
            TabCommand::Inspect(_) => Some(ActionKind::TabInspect),
            TabCommand::Create(_) => Some(ActionKind::TabCreate),
            TabCommand::Activate(_) => Some(ActionKind::TabActivate),
            TabCommand::Move(_) => Some(ActionKind::TabMove),
            TabCommand::Close(_) => Some(ActionKind::TabClose),
            TabCommand::Rename(_) => Some(ActionKind::TabRename),
            TabCommand::ResetName(_) => Some(ActionKind::TabResetName),
            TabCommand::Color(command) => match command {
                TabColorCommand::Set(_) => Some(ActionKind::TabColorSet),
                TabColorCommand::Clear(_) => Some(ActionKind::TabColorClear),
            },
        },
        ControlCommand::Pane(command) => match command {
            PaneCommand::List(_) => Some(ActionKind::PaneList),
            PaneCommand::Inspect(_) => Some(ActionKind::PaneInspect),
            PaneCommand::Split(_) => Some(ActionKind::PaneSplit),
            PaneCommand::Focus(_) => Some(ActionKind::PaneFocus),
            PaneCommand::Navigate(_) => Some(ActionKind::PaneNavigate),
            PaneCommand::Resize(_) => Some(ActionKind::PaneResize),
            PaneCommand::Maximize(_) => Some(ActionKind::PaneMaximize),
            PaneCommand::Unmaximize(_) => Some(ActionKind::PaneUnmaximize),
            PaneCommand::Close(_) => Some(ActionKind::PaneClose),
            PaneCommand::Rename(_) => Some(ActionKind::PaneRename),
            PaneCommand::ResetName(_) => Some(ActionKind::PaneResetName),
        },
        ControlCommand::Session(command) => match command {
            SessionCommand::List(_) => Some(ActionKind::SessionList),
            SessionCommand::Inspect(_) => Some(ActionKind::SessionInspect),
            SessionCommand::Activate(_) => Some(ActionKind::SessionActivate),
            SessionCommand::Previous(_) => Some(ActionKind::SessionPrevious),
            SessionCommand::Next(_) => Some(ActionKind::SessionNext),
            SessionCommand::ReopenClosed(_) => Some(ActionKind::SessionReopenClosed),
        },
        ControlCommand::Input(command) => match command {
            InputCommand::Insert(_) => Some(ActionKind::InputInsert),
            InputCommand::Replace(_) => Some(ActionKind::InputReplace),
        },
        ControlCommand::Theme(command) => match command {
            ThemeCommand::List(_) => Some(ActionKind::ThemeList),
            ThemeCommand::Get(_) => Some(ActionKind::ThemeGet),
            ThemeCommand::Set(_) => Some(ActionKind::ThemeSet),
            ThemeCommand::SystemSet(_) => Some(ActionKind::ThemeSystemSet),
            ThemeCommand::LightSet(_) => Some(ActionKind::ThemeLightSet),
            ThemeCommand::DarkSet(_) => Some(ActionKind::ThemeDarkSet),
        },
        ControlCommand::Appearance(command) => match command {
            AppearanceCommand::Get(_) => Some(ActionKind::AppearanceGet),
            AppearanceCommand::FontSizeIncrease(_) => Some(ActionKind::AppearanceFontSizeIncrease),
            AppearanceCommand::FontSizeDecrease(_) => Some(ActionKind::AppearanceFontSizeDecrease),
            AppearanceCommand::FontSizeReset(_) => Some(ActionKind::AppearanceFontSizeReset),
            AppearanceCommand::ZoomIncrease(_) => Some(ActionKind::AppearanceZoomIncrease),
            AppearanceCommand::ZoomDecrease(_) => Some(ActionKind::AppearanceZoomDecrease),
            AppearanceCommand::ZoomReset(_) => Some(ActionKind::AppearanceZoomReset),
        },
        ControlCommand::Setting(command) => match command {
            SettingCommand::List(_) => Some(ActionKind::SettingList),
            SettingCommand::Get(_) => Some(ActionKind::SettingGet),
            SettingCommand::Set(_) => Some(ActionKind::SettingSet),
            SettingCommand::Toggle(_) => Some(ActionKind::SettingToggle),
        },
        ControlCommand::Keybinding(command) => match command {
            KeybindingCommand::List(_) => Some(ActionKind::KeybindingList),
            KeybindingCommand::Get(_) => Some(ActionKind::KeybindingGet),
        },
        ControlCommand::File(command) => match command {
            FileCommand::Open(_) => Some(ActionKind::FileOpen),
        },
        ControlCommand::Surface(command) => match command {
            SurfaceCommand::List(_) => Some(ActionKind::SurfaceList),
            SurfaceCommand::Settings(command) => match command {
                SurfaceSettingsCommand::Open(_) => Some(ActionKind::SurfaceSettingsOpen),
            },
            SurfaceCommand::CommandPalette(command) => match command {
                SurfaceQueryCommand::Open(_) => Some(ActionKind::SurfaceCommandPaletteOpen),
            },
            SurfaceCommand::CommandSearch(command) => match command {
                SurfaceQueryCommand::Open(_) => Some(ActionKind::SurfaceCommandSearchOpen),
            },
            SurfaceCommand::ThemePicker(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceThemePickerOpen),
            },
            SurfaceCommand::Keybindings(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceKeybindingsOpen),
            },
            SurfaceCommand::BorkDrive(command) => match command {
                SurfaceOpenToggleCommand::Open(_) => Some(ActionKind::SurfaceBorkDriveOpen),
                SurfaceOpenToggleCommand::Toggle(_) => Some(ActionKind::SurfaceBorkDriveToggle),
            },
            SurfaceCommand::ResourceCenter(command) => match command {
                SurfaceToggleCommand::Toggle(_) => Some(ActionKind::SurfaceResourceCenterToggle),
            },
            SurfaceCommand::AiAssistant(command) => match command {
                SurfaceToggleCommand::Toggle(_) => Some(ActionKind::SurfaceAiAssistantToggle),
            },
            SurfaceCommand::CodeReview(command) => match command {
                SurfaceOpenToggleCommand::Open(_) => Some(ActionKind::SurfaceCodeReviewOpen),
                SurfaceOpenToggleCommand::Toggle(_) => Some(ActionKind::SurfaceCodeReviewToggle),
            },
            SurfaceCommand::ProjectExplorer(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceProjectExplorerOpen),
            },
            SurfaceCommand::GlobalSearch(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceGlobalSearchOpen),
            },
            SurfaceCommand::ConversationList(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceConversationListOpen),
            },
            SurfaceCommand::LeftPanel(command) => match command {
                SurfaceToggleCommand::Toggle(_) => Some(ActionKind::SurfaceLeftPanelToggle),
            },
            SurfaceCommand::RightPanel(command) => match command {
                SurfaceToggleCommand::Toggle(_) => Some(ActionKind::SurfaceRightPanelToggle),
            },
            SurfaceCommand::VerticalTabs(command) => match command {
                SurfaceOpenToggleCommand::Open(_) => Some(ActionKind::SurfaceVerticalTabsOpen),
                SurfaceOpenToggleCommand::Toggle(_) => Some(ActionKind::SurfaceVerticalTabsToggle),
            },
            SurfaceCommand::AgentManagement(command) => match command {
                SurfaceOpenCommand::Open(_) => Some(ActionKind::SurfaceAgentManagementOpen),
            },
        },
        ControlCommand::Completions { .. } => None,
    }
}
