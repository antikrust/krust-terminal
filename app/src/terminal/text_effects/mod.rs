mod picker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use instant::Instant;
pub use picker::{TextEffectPicker, TextEffectPickerEvent};
use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{Setting as _, SupportedPlatforms, SyncToCloud};
use warp_terminal::model::ansi::{Color, NamedColor};
use warp_terminal::model::char_or_str::CharOrStr;
use warpui::color::ColorU;
use warpui::{AppContext, SingletonEntity};

use crate::features::FeatureFlag;
use crate::terminal::model::grid::grid_handler::AbsolutePoint;

const MAX_PENDING_WRITES: usize = 4_096;
const MAX_ACTIVE_BATCHES: usize = 8;
const MATRIX_STREAM_CONTINUATION_WINDOW: Duration = Duration::from_millis(250);
const MATRIX_GLYPHS: &[u8] = b"!@#$%^&*+-=<>?/\\|:;";
const AUTOMATIC_SPINNER_LINGER: Duration = Duration::from_millis(350);
const MAX_AUTOMATIC_SPINNER_DURATION: Duration = Duration::from_secs(4);
const MAX_ACTIVE_SPINNERS: usize = 64;
const MIN_SPINNER_FPS: u8 = 1;
const MAX_SPINNER_FPS: u8 = 60;

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The animation used for newly rendered terminal text.")]
pub enum TextEffect {
    Immediate,
    WordReveal,
    #[default]
    Typewriter,
    MatrixTrail,
    Decode,
    CasePulse,
    WordDecode,
}

impl TextEffect {
    pub const VALUES: [Self; 7] = [
        Self::Immediate,
        Self::WordReveal,
        Self::Typewriter,
        Self::MatrixTrail,
        Self::Decode,
        Self::CasePulse,
        Self::WordDecode,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Immediate => "Immediate",
            Self::WordReveal => "Word reveal",
            Self::Typewriter => "Typewriter",
            Self::MatrixTrail => "Matrix trail",
            Self::Decode => "Decode",
            Self::CasePulse => "Case pulse",
            Self::WordDecode => "Word decode",
        }
    }

    pub fn uses_speed(self) -> bool {
        self != Self::Immediate
    }

    pub fn uses_intensity(self) -> bool {
        matches!(self, Self::MatrixTrail | Self::Decode | Self::WordDecode)
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The colour treatment layered over terminal text animations.")]
pub enum TextEffectColor {
    #[default]
    Terminal,
    Grey,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    ClassicCycle,
    Rainbow,
}

impl TextEffectColor {
    pub const VALUES: [Self; 11] = [
        Self::Terminal,
        Self::Grey,
        Self::Red,
        Self::Green,
        Self::Yellow,
        Self::Blue,
        Self::Magenta,
        Self::Cyan,
        Self::White,
        Self::ClassicCycle,
        Self::Rainbow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal colours",
            Self::Grey => "Grey",
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Yellow => "Yellow",
            Self::Blue => "Blue",
            Self::Magenta => "Magenta",
            Self::Cyan => "Cyan",
            Self::White => "White",
            Self::ClassicCycle => "Classic txtFX cycle",
            Self::Rainbow => "Rainbow",
        }
    }

    pub fn is_dynamic(self) -> bool {
        matches!(self, Self::ClassicCycle | Self::Rainbow)
    }

    pub fn is_enabled(self) -> bool {
        self != Self::Terminal
    }

    fn solid_color(self) -> Option<TextFxNamedColor> {
        match self {
            Self::Terminal | Self::ClassicCycle | Self::Rainbow => None,
            Self::Grey => Some(TextFxNamedColor::Grey),
            Self::Red => Some(TextFxNamedColor::Red),
            Self::Green => Some(TextFxNamedColor::Green),
            Self::Yellow => Some(TextFxNamedColor::Yellow),
            Self::Blue => Some(TextFxNamedColor::Blue),
            Self::Magenta => Some(TextFxNamedColor::Magenta),
            Self::Cyan => Some(TextFxNamedColor::Cyan),
            Self::White => Some(TextFxNamedColor::White),
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The speed of animated txtFX colour treatments.")]
pub enum TextEffectColorSpeed {
    Slow,
    #[default]
    Normal,
    Fast,
}

impl TextEffectColorSpeed {
    pub const VALUES: [Self; 3] = [Self::Slow, Self::Normal, Self::Fast];

    pub fn label(self) -> &'static str {
        match self {
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
        }
    }

    fn step_duration(self) -> Duration {
        match self {
            Self::Slow => Duration::from_millis(140),
            Self::Normal => Duration::from_millis(70),
            Self::Fast => Duration::from_millis(35),
        }
    }

    fn frame_interval(self) -> Duration {
        match self {
            Self::Slow => Duration::from_millis(33),
            Self::Normal | Self::Fast => Duration::from_millis(16),
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The brightness of txtFX colour treatments.")]
pub enum TextEffectColorIntensity {
    Subtle,
    #[default]
    Balanced,
    Vivid,
}

impl TextEffectColorIntensity {
    pub const VALUES: [Self; 3] = [Self::Subtle, Self::Balanced, Self::Vivid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Subtle => "Subtle",
            Self::Balanced => "Balanced",
            Self::Vivid => "Vivid",
        }
    }

    fn rainbow_levels(self) -> (u16, u16) {
        match self {
            Self::Subtle => (150, 175),
            Self::Balanced => (205, 225),
            Self::Vivid => (255, 255),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum TextFxNamedColor {
    Grey,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl TextFxNamedColor {
    fn ansi_color(self, intensity: TextEffectColorIntensity) -> NamedColor {
        match intensity {
            TextEffectColorIntensity::Subtle => match self {
                Self::Grey => NamedColor::DimWhite,
                Self::Red => NamedColor::DimRed,
                Self::Green => NamedColor::DimGreen,
                Self::Yellow => NamedColor::DimYellow,
                Self::Blue => NamedColor::DimBlue,
                Self::Magenta => NamedColor::DimMagenta,
                Self::Cyan => NamedColor::DimCyan,
                Self::White => NamedColor::DimWhite,
            },
            TextEffectColorIntensity::Balanced => match self {
                Self::Grey => NamedColor::BrightBlack,
                Self::Red => NamedColor::Red,
                Self::Green => NamedColor::Green,
                Self::Yellow => NamedColor::Yellow,
                Self::Blue => NamedColor::Blue,
                Self::Magenta => NamedColor::Magenta,
                Self::Cyan => NamedColor::Cyan,
                Self::White => NamedColor::White,
            },
            TextEffectColorIntensity::Vivid => match self {
                Self::Grey => NamedColor::White,
                Self::Red => NamedColor::BrightRed,
                Self::Green => NamedColor::BrightGreen,
                Self::Yellow => NamedColor::BrightYellow,
                Self::Blue => NamedColor::BrightBlue,
                Self::Magenta => NamedColor::BrightMagenta,
                Self::Cyan => NamedColor::BrightCyan,
                Self::White => NamedColor::BrightWhite,
            },
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The speed of terminal text animations.")]
pub enum TextEffectSpeed {
    Slow,
    #[default]
    Normal,
    Fast,
}

impl TextEffectSpeed {
    pub const VALUES: [Self; 3] = [Self::Slow, Self::Normal, Self::Fast];

    pub fn label(self) -> &'static str {
        match self {
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
        }
    }

    fn step_duration(self) -> Duration {
        match self {
            Self::Slow => Duration::from_millis(70),
            Self::Normal => Duration::from_millis(35),
            Self::Fast => Duration::from_millis(16),
        }
    }

    pub fn frame_interval(self) -> Duration {
        match self {
            Self::Slow => Duration::from_millis(33),
            Self::Normal | Self::Fast => Duration::from_millis(16),
        }
    }

    fn matrix_line_duration(self) -> Duration {
        match self {
            Self::Slow => Duration::from_millis(33),
            Self::Normal => Duration::from_millis(16),
            Self::Fast => Duration::from_millis(8),
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The visual intensity of terminal text animations.")]
pub enum TextEffectIntensity {
    Subtle,
    #[default]
    Balanced,
    Strong,
}

impl TextEffectIntensity {
    pub const VALUES: [Self; 3] = [Self::Subtle, Self::Balanced, Self::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Self::Subtle => "Subtle",
            Self::Balanced => "Balanced",
            Self::Strong => "Strong",
        }
    }

    fn trail_cells(self) -> usize {
        match self {
            Self::Subtle => 3,
            Self::Balanced => 6,
            Self::Strong => 12,
        }
    }

    fn matrix_trail_lines(self) -> usize {
        match self {
            Self::Subtle => 1,
            Self::Balanced => 2,
            Self::Strong => 4,
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The terminal surfaces on which text animations are shown.")]
pub enum TextEffectScope {
    #[default]
    OutputOnly,
    OutputAndCommands,
    AllTerminalScreens,
}

impl TextEffectScope {
    pub const VALUES: [Self; 3] = [
        Self::OutputOnly,
        Self::OutputAndCommands,
        Self::AllTerminalScreens,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OutputOnly => "Command output",
            Self::OutputAndCommands => "Output and commands",
            Self::AllTerminalScreens => "All terminal screens",
        }
    }

    fn includes(self, surface: TextEffectSurface) -> bool {
        match self {
            Self::OutputOnly => surface == TextEffectSurface::Output,
            Self::OutputAndCommands => {
                matches!(
                    surface,
                    TextEffectSurface::Output | TextEffectSurface::Command
                )
            }
            Self::AllTerminalScreens => true,
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "How txtFX handles unusually large output batches.")]
pub enum TextEffectLargeOutput {
    #[default]
    KeepInstant,
    Animate,
}

impl TextEffectLargeOutput {
    pub const VALUES: [Self; 2] = [Self::KeepInstant, Self::Animate];

    pub fn label(self) -> &'static str {
        match self {
            Self::KeepInstant => "Keep large output instant",
            Self::Animate => "Animate all output",
        }
    }

    fn should_animate(self, cell_count: usize) -> bool {
        self == Self::Animate || cell_count <= 1_024
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TextEffectSurface {
    Output,
    Command,
    AlternateScreen,
}

#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
#[schemars(description = "The optional spinner shown beside newly printed txtFX text.")]
pub enum SpinnerStyle {
    #[default]
    Off,
    Line,
    Orbit,
    Arrows,
    Quarters,
    Fill,
    Clock,
    Bar,
    Bounce,
}

impl SpinnerStyle {
    pub const VALUES: [Self; 9] = [
        Self::Off,
        Self::Line,
        Self::Orbit,
        Self::Arrows,
        Self::Quarters,
        Self::Fill,
        Self::Clock,
        Self::Bar,
        Self::Bounce,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Line => "Line  | / - \\",
            Self::Orbit => "Orbit  . o 0 O",
            Self::Arrows => "Arrows  ⇐ ⇖ ⇑ ⇗",
            Self::Quarters => "Quarters  ◓ ◑ ◒ ◐",
            Self::Fill => "Fill  ○ ◔ ◑ ◕ ●",
            Self::Clock => "Clock  ◴ ◷ ◶ ◵",
            Self::Bar => "Bar  ▏▎▍▌▋▊▉█",
            Self::Bounce => "Bounce  ▁▂▃▄▅▆▇█",
        }
    }

    pub fn is_enabled(self) -> bool {
        self != Self::Off
    }

    pub fn frames(self) -> &'static str {
        match self {
            Self::Off => " ",
            Self::Line => "|/-\\",
            Self::Orbit => ".o0O0o. ",
            Self::Arrows => "⇐⇖⇑⇗⇒⇘⇓⇙",
            Self::Quarters => "◓◑◒◐",
            Self::Fill => "○◔◑◕●",
            Self::Clock => "◴◷◶◵",
            Self::Bar => "▏▎▍▌▋▊▉█▉▊▌▍▎",
            Self::Bounce => "▁▂▃▄▅▆▇█▇▆▅▄▃▂",
        }
    }

    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"1" | b"line" => Some(Self::Line),
            b"2" | b"orbit" => Some(Self::Orbit),
            b"3" | b"arrows" => Some(Self::Arrows),
            b"4" | b"quarters" => Some(Self::Quarters),
            b"5" | b"fill" => Some(Self::Fill),
            b"6" | b"clock" => Some(Self::Clock),
            b"7" | b"bar" => Some(Self::Bar),
            b"8" | b"bounce" => Some(Self::Bounce),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEffectsCommand {
    SpinnerStart {
        id: String,
        style: SpinnerStyle,
        fps: u8,
    },
    SpinnerUpdate {
        id: String,
        style: SpinnerStyle,
        fps: u8,
    },
    SpinnerStop {
        id: String,
    },
}

impl TextEffectsCommand {
    pub fn parse_osc_params(params: &[&[u8]]) -> Option<Self> {
        if params.first().copied() != Some(b"spinner") {
            return None;
        }

        let operation = *params.get(1)?;
        let id = parse_spinner_id(params.get(2)?)?;
        match operation {
            b"start" | b"update" => {
                let style = SpinnerStyle::parse(params.get(3)?)?;
                let fps = std::str::from_utf8(params.get(4).copied().unwrap_or(b"20"))
                    .ok()?
                    .parse::<u8>()
                    .ok()?
                    .clamp(MIN_SPINNER_FPS, MAX_SPINNER_FPS);
                if operation == b"start" {
                    Some(Self::SpinnerStart { id, style, fps })
                } else {
                    Some(Self::SpinnerUpdate { id, style, fps })
                }
            }
            b"stop" => Some(Self::SpinnerStop { id }),
            _ => None,
        }
    }
}

fn parse_spinner_id(value: &[u8]) -> Option<String> {
    let id = std::str::from_utf8(value).ok()?;
    (!id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then(|| id.to_owned())
}

define_settings_group!(TextEffectsSettings, settings: [
    enabled: TextEffectsEnabled {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.enabled",
        description: "Whether newly printed terminal output uses a visual text effect.",
    },
    effect: SelectedTextEffect {
        type: TextEffect,
        default: TextEffect::Typewriter,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.effect",
        description: "The visual effect used for newly printed terminal output.",
    },
    speed: SelectedTextEffectSpeed {
        type: TextEffectSpeed,
        default: TextEffectSpeed::Normal,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.speed",
        description: "The speed of terminal text effects.",
    },
    spinner: SelectedTextEffectSpinner {
        type: SpinnerStyle,
        default: SpinnerStyle::Off,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.spinner",
        description: "The spinner shown beside text while new output is being printed.",
    },
    intensity: SelectedTextEffectIntensity {
        type: TextEffectIntensity,
        default: TextEffectIntensity::Balanced,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.intensity",
        description: "The visual intensity of terminal text effects.",
    },
    color: SelectedTextEffectColor {
        type: TextEffectColor,
        default: TextEffectColor::Terminal,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.color",
        description: "The colour treatment layered independently over terminal text effects.",
    },
    color_speed: SelectedTextEffectColorSpeed {
        type: TextEffectColorSpeed,
        default: TextEffectColorSpeed::Normal,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.color_speed",
        description: "The speed of animated terminal text colour treatments.",
    },
    color_intensity: SelectedTextEffectColorIntensity {
        type: TextEffectColorIntensity,
        default: TextEffectColorIntensity::Balanced,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.color_intensity",
        description: "The brightness of terminal text colour treatments.",
    },
    scope: SelectedTextEffectScope {
        type: TextEffectScope,
        default: TextEffectScope::OutputOnly,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.scope",
        description: "The terminal surfaces on which text effects are shown.",
    },
    large_output: SelectedTextEffectLargeOutput {
        type: TextEffectLargeOutput,
        default: TextEffectLargeOutput::KeepInstant,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.large_output",
        description: "Whether unusually large output batches should be animated.",
    },
    agent_mode: ApplyTextEffectsToAgentMode {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.agent_mode",
        description: "Whether newly streamed Agent Mode response text uses txtFX.",
    },
    show_title_bar_picker: ShowTextEffectsTitleBarPicker {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "appearance.txtfx.show_title_bar_picker",
        description: "Whether to show the txtFX picker in the title bar.",
    },
]);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TextEffectsConfig {
    pub effect: TextEffect,
    pub speed: TextEffectSpeed,
    pub spinner: SpinnerStyle,
    pub intensity: TextEffectIntensity,
    pub color: TextEffectColor,
    pub color_speed: TextEffectColorSpeed,
    pub color_intensity: TextEffectColorIntensity,
    pub scope: TextEffectScope,
    pub large_output: TextEffectLargeOutput,
}

impl TextEffectsConfig {
    pub fn from_app(app: &AppContext) -> Option<Self> {
        if !FeatureFlag::TextEffects.is_enabled() {
            return None;
        }

        let settings = TextEffectsSettings::as_ref(app);
        if !*settings.enabled.value() {
            return None;
        }

        Some(Self {
            effect: *settings.effect.value(),
            speed: *settings.speed.value(),
            spinner: *settings.spinner.value(),
            intensity: *settings.intensity.value(),
            color: *settings.color.value(),
            color_speed: *settings.color_speed.value(),
            color_intensity: *settings.color_intensity.value(),
            scope: *settings.scope.value(),
            large_output: *settings.large_output.value(),
        })
    }

    pub fn for_surface(app: &AppContext, surface: TextEffectSurface) -> Option<Self> {
        Self::from_app(app).filter(|config| config.scope.includes(surface))
    }

    pub fn for_agent_mode(app: &AppContext) -> Option<Self> {
        Self::from_app(app).filter(|_| *TextEffectsSettings::as_ref(app).agent_mode.value())
    }

    pub fn frame_interval(self) -> Duration {
        if self.color.is_dynamic() {
            self.speed
                .frame_interval()
                .min(self.color_speed.frame_interval())
        } else {
            self.speed.frame_interval()
        }
    }
}

pub fn should_use_text_effects(app: &AppContext) -> bool {
    TextEffectsConfig::from_app(app).is_some()
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VisualContent {
    Original,
    Hidden,
    Substitute(char),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TextEffectSample {
    pub content: VisualContent,
    pub foreground: Option<Color>,
    pub needs_repaint: bool,
}

impl TextEffectSample {
    const COMPLETE: Self = Self {
        content: VisualContent::Original,
        foreground: None,
        needs_repaint: false,
    };
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TextEffectSpinnerSample {
    pub character: char,
    pub foreground: Option<Color>,
    pub needs_repaint: bool,
}

#[derive(Clone, Debug)]
struct AnimatedCell {
    point: AbsolutePoint,
    fingerprint: String,
    ordinal: usize,
    line_ordinal: usize,
    word_ordinal: usize,
    ordinal_in_word: usize,
    word_len: usize,
}

#[derive(Clone, Debug)]
struct TextEffectBatch {
    started_at: Instant,
    matrix_started_at: Instant,
    matrix_line_offset: usize,
    seed: u64,
    cells: Vec<AnimatedCell>,
    line_count: usize,
    word_count: usize,
}

#[derive(Clone, Debug)]
struct SpinnerInstance {
    point: AbsolutePoint,
    fingerprint: Option<String>,
    started_at: Instant,
    style: SpinnerStyle,
    fps: u8,
}

#[derive(Clone, Debug)]
struct AutomaticSpinner {
    point: AbsolutePoint,
    fingerprint: String,
    started_at: Instant,
    updated_at: Instant,
}

impl TextEffectBatch {
    fn cell(&self, point: AbsolutePoint) -> Option<&AnimatedCell> {
        self.cells.iter().find(|cell| cell.point == point)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TextEffectsTimeline {
    pending_writes: Vec<AbsolutePoint>,
    batches: VecDeque<TextEffectBatch>,
    next_seed: u64,
    spinners: HashMap<String, SpinnerInstance>,
    automatic_spinner: Option<AutomaticSpinner>,
}

impl TextEffectsTimeline {
    pub fn clear(&mut self) {
        self.pending_writes.clear();
        self.batches.clear();
        self.spinners.clear();
        self.automatic_spinner = None;
    }

    pub fn record_write(&mut self, point: AbsolutePoint) {
        if self.pending_writes.len() < MAX_PENDING_WRITES {
            self.pending_writes.push(point);
        }
    }

    pub fn take_pending_writes(&mut self) -> Vec<AbsolutePoint> {
        let mut seen = HashSet::new();
        let mut writes: Vec<_> = self
            .pending_writes
            .drain(..)
            .rev()
            .filter(|point| seen.insert(*point))
            .collect();
        writes.reverse();
        writes
    }

    pub fn finish_batch(
        &mut self,
        written_cells: impl IntoIterator<Item = (AbsolutePoint, String)>,
        started_at: Instant,
    ) {
        let written_cells: Vec<_> = written_cells.into_iter().collect();
        if written_cells.is_empty() {
            return;
        }

        let latest_row = written_cells
            .last()
            .expect("non-empty text effect batches have a final cell")
            .0
            .row;
        let (spinner_point, spinner_fingerprint) = written_cells
            .iter()
            .find(|(point, _)| point.row == latest_row)
            .expect("the final row contains at least one written cell");
        let existing_spinner_was_overwritten =
            self.automatic_spinner.as_ref().is_some_and(|spinner| {
                written_cells.iter().any(|(point, fingerprint)| {
                    *point == spinner.point && *fingerprint != spinner.fingerprint
                })
            });
        let continues_current_line = self.automatic_spinner.as_ref().is_some_and(|spinner| {
            !existing_spinner_was_overwritten
                && spinner.point.row == latest_row
                && started_at.saturating_duration_since(spinner.updated_at)
                    <= MATRIX_STREAM_CONTINUATION_WINDOW
        });
        if continues_current_line {
            if let Some(spinner) = &mut self.automatic_spinner {
                spinner.updated_at = started_at;
            }
        } else {
            self.automatic_spinner = Some(AutomaticSpinner {
                point: *spinner_point,
                fingerprint: spinner_fingerprint.clone(),
                started_at,
                updated_at: started_at,
            });
        }

        let mut spinners_to_remove = Vec::new();
        for (point, fingerprint) in &written_cells {
            for (id, spinner) in &mut self.spinners {
                if spinner.point != *point {
                    continue;
                }
                match &spinner.fingerprint {
                    None => spinner.fingerprint = Some(fingerprint.clone()),
                    Some(existing) if existing != fingerprint => {
                        spinners_to_remove.push(id.clone())
                    }
                    Some(_) => {}
                }
            }
        }
        for id in spinners_to_remove {
            self.spinners.remove(&id);
        }

        let mut word_ordinal = 0;
        let mut in_word = false;
        let mut word_positions = Vec::with_capacity(written_cells.len());
        let mut word_lengths = Vec::new();
        let mut line_ordinals = HashMap::new();
        let mut line_positions = Vec::with_capacity(written_cells.len());

        for (point, content) in &written_cells {
            let next_line_ordinal = line_ordinals.len();
            let line_ordinal = *line_ordinals.entry(point.row).or_insert(next_line_ordinal);
            line_positions.push(line_ordinal);

            let is_whitespace = content.chars().all(char::is_whitespace);
            if is_whitespace {
                in_word = false;
                word_positions.push((word_ordinal, 0));
                continue;
            }

            if !in_word {
                if !word_lengths.is_empty() {
                    word_ordinal += 1;
                }
                word_lengths.push(0);
                in_word = true;
            }

            let ordinal_in_word = word_lengths[word_ordinal];
            word_lengths[word_ordinal] += 1;
            word_positions.push((word_ordinal, ordinal_in_word));
        }

        let cells = written_cells
            .into_iter()
            .zip(word_positions)
            .zip(line_positions)
            .enumerate()
            .map(
                |(
                    ordinal,
                    (((point, fingerprint), (word_ordinal, ordinal_in_word)), line_ordinal),
                )| {
                    let word_len = word_lengths.get(word_ordinal).copied().unwrap_or_default();
                    AnimatedCell {
                        point,
                        fingerprint,
                        ordinal,
                        line_ordinal,
                        word_ordinal,
                        ordinal_in_word,
                        word_len,
                    }
                },
            )
            .collect::<Vec<_>>();

        let (matrix_started_at, matrix_line_offset) = self
            .batches
            .back()
            .filter(|batch| {
                started_at.saturating_duration_since(batch.started_at)
                    <= MATRIX_STREAM_CONTINUATION_WINDOW
            })
            .map(|batch| {
                let continues_last_line = batch
                    .cells
                    .last()
                    .zip(cells.first())
                    .is_some_and(|(previous, current)| previous.point.row == current.point.row);
                (
                    batch.matrix_started_at,
                    batch.matrix_line_offset + batch.line_count - usize::from(continues_last_line),
                )
            })
            .unwrap_or((started_at, 0));

        self.next_seed = self.next_seed.wrapping_add(1);
        self.batches.push_back(TextEffectBatch {
            started_at,
            matrix_started_at,
            matrix_line_offset,
            seed: self.next_seed,
            cells,
            line_count: line_ordinals.len(),
            word_count: word_lengths.len(),
        });

        while self.batches.len() > MAX_ACTIVE_BATCHES {
            self.batches.pop_front();
        }
    }

    pub fn sample(
        &self,
        point: AbsolutePoint,
        original_content: CharOrStr<'_>,
        config: TextEffectsConfig,
        now: Instant,
    ) -> Option<TextEffectSample> {
        if let Some(spinner) = self.spinners.values().find(|spinner| {
            spinner.point == point
                && spinner
                    .fingerprint
                    .as_deref()
                    .is_some_and(|fingerprint| content_matches(fingerprint, original_content))
        }) {
            let elapsed = now.saturating_duration_since(spinner.started_at);
            let frame = elapsed.as_millis() * u128::from(spinner.fps) / 1_000;
            let frames = spinner.style.frames();
            let frame = usize::try_from(frame).unwrap_or(usize::MAX) % frames.chars().count();
            return Some(TextEffectSample {
                content: VisualContent::Substitute(
                    frames
                        .chars()
                        .nth(frame)
                        .expect("spinner styles must contain at least one frame"),
                ),
                foreground: sample_foreground(config, point.col, elapsed),
                needs_repaint: true,
            });
        }

        if config.effect == TextEffect::Immediate && !config.color.is_enabled() {
            return None;
        }

        let (batch, cell) = self.batches.iter().rev().find_map(|batch| {
            batch
                .cell(point)
                .filter(|cell| content_matches(&cell.fingerprint, original_content))
                .map(|cell| (batch, cell))
        })?;

        if !config.large_output.should_animate(batch.cells.len()) {
            return None;
        }

        Some(sample_cell(batch, cell, config, now))
    }

    pub fn sample_spinner(
        &self,
        point: AbsolutePoint,
        original_content: CharOrStr<'_>,
        config: TextEffectsConfig,
        now: Instant,
    ) -> Option<TextEffectSpinnerSample> {
        let spinner = self.automatic_spinner.as_ref()?;
        if spinner.point != point || !content_matches(&spinner.fingerprint, original_content) {
            return None;
        }
        self.automatic_spinner_sample(config, now)
    }

    fn automatic_spinner_sample(
        &self,
        config: TextEffectsConfig,
        now: Instant,
    ) -> Option<TextEffectSpinnerSample> {
        if !config.spinner.is_enabled() {
            return None;
        }

        let spinner = self.automatic_spinner.as_ref()?;
        let recently_updated =
            now.saturating_duration_since(spinner.updated_at) <= AUTOMATIC_SPINNER_LINGER;
        let text_animation_active = self.batches.back().is_some_and(|batch| {
            now.saturating_duration_since(batch.started_at)
                <= text_effect_animation_duration(batch, config).min(MAX_AUTOMATIC_SPINNER_DURATION)
        });
        if !recently_updated && !text_animation_active {
            return None;
        }

        let elapsed = now.saturating_duration_since(spinner.started_at);
        let frame_duration = config.speed.step_duration() * 2;
        let frame = elapsed.as_nanos() / frame_duration.as_nanos().max(1);
        let frame = usize::try_from(frame).unwrap_or(usize::MAX);
        let frames = config.spinner.frames();
        let character = frames
            .chars()
            .nth(frame % frames.chars().count())
            .expect("enabled spinner styles contain at least one frame");
        Some(TextEffectSpinnerSample {
            character,
            foreground: sample_foreground(config, 0, elapsed),
            needs_repaint: true,
        })
    }

    pub fn apply_command(
        &mut self,
        command: TextEffectsCommand,
        cursor_point: AbsolutePoint,
        now: Instant,
    ) {
        match command {
            TextEffectsCommand::SpinnerStart { id, style, fps } => {
                if self.spinners.len() >= MAX_ACTIVE_SPINNERS && !self.spinners.contains_key(&id) {
                    return;
                }
                self.spinners.insert(
                    id,
                    SpinnerInstance {
                        point: cursor_point,
                        fingerprint: None,
                        started_at: now,
                        style,
                        fps,
                    },
                );
            }
            TextEffectsCommand::SpinnerUpdate { id, style, fps } => {
                if let Some(spinner) = self.spinners.get_mut(&id) {
                    spinner.style = style;
                    spinner.fps = fps;
                    spinner.started_at = now;
                }
            }
            TextEffectsCommand::SpinnerStop { id } => {
                self.spinners.remove(&id);
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct AgentTextSectionTimeline {
    observed_text: String,
    points: Vec<Option<AbsolutePoint>>,
    next_row: u64,
    timeline: TextEffectsTimeline,
}

#[derive(Clone, Debug, Default)]
pub struct AgentTextEffectsTimeline {
    sections: HashMap<usize, AgentTextSectionTimeline>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTextEffectFrame {
    pub samples: Vec<Option<TextEffectSample>>,
    pub spinner: Option<TextEffectSpinnerSample>,
    pub needs_repaint: bool,
}

impl AgentTextEffectsTimeline {
    pub fn clear(&mut self) {
        self.sections.clear();
    }

    pub fn contains_section(&self, section_index: usize) -> bool {
        self.sections.contains_key(&section_index)
    }

    pub fn frame(
        &mut self,
        section_index: usize,
        text: &str,
        config: TextEffectsConfig,
        now: Instant,
    ) -> AgentTextEffectFrame {
        let section = self.sections.entry(section_index).or_default();
        if section.observed_text != text {
            let is_append = text.starts_with(&section.observed_text);
            if !is_append {
                section.timeline.clear();
                section.points.clear();
                section.next_row = 0;
                section.observed_text.clear();
            }

            let appended_text = if is_append {
                &text[section.observed_text.len()..]
            } else {
                text
            };
            let mut row = section.next_row;
            let mut col = 0;
            let mut written_cells = Vec::new();
            for character in appended_text.chars() {
                if matches!(character, '\n' | '\r') {
                    section.points.push(None);
                    row = row.saturating_add(1);
                    col = 0;
                    continue;
                }

                let point = AbsolutePoint { row, col };
                section.points.push(Some(point));
                written_cells.push((point, character.to_string()));
                col += 1;
            }
            section.next_row = row.saturating_add(1);
            section.timeline.finish_batch(written_cells, now);
            section.observed_text = text.to_owned();
        }

        let samples = text
            .chars()
            .zip(&section.points)
            .map(|(character, point)| {
                point.and_then(|point| {
                    section
                        .timeline
                        .sample(point, CharOrStr::Char(character), config, now)
                })
            })
            .collect::<Vec<_>>();
        let spinner = section.timeline.automatic_spinner_sample(config, now);
        let needs_repaint = samples.iter().flatten().any(|sample| sample.needs_repaint)
            || spinner.is_some_and(|spinner| spinner.needs_repaint);
        AgentTextEffectFrame {
            samples,
            spinner,
            needs_repaint,
        }
    }

    pub fn needs_repaint(&self, config: TextEffectsConfig, now: Instant) -> bool {
        self.sections.values().any(|section| {
            section
                .timeline
                .automatic_spinner_sample(config, now)
                .is_some()
                || section
                    .observed_text
                    .chars()
                    .zip(&section.points)
                    .any(|(character, point)| {
                        point.is_some_and(|point| {
                            section
                                .timeline
                                .sample(point, CharOrStr::Char(character), config, now)
                                .is_some_and(|sample| sample.needs_repaint)
                        })
                    })
        })
    }
}

fn content_matches(fingerprint: &str, content: CharOrStr<'_>) -> bool {
    match content {
        CharOrStr::Char(character) => {
            let mut buffer = [0; 4];
            fingerprint == character.encode_utf8(&mut buffer)
        }
        CharOrStr::Str(content) => fingerprint == content,
    }
}

fn text_effect_animation_duration(batch: &TextEffectBatch, config: TextEffectsConfig) -> Duration {
    let steps = match config.effect {
        TextEffect::Immediate => 0,
        TextEffect::WordReveal => batch.word_count,
        TextEffect::Typewriter | TextEffect::Decode => batch.cells.len(),
        TextEffect::MatrixTrail => batch.line_count + config.intensity.matrix_trail_lines(),
        TextEffect::CasePulse => batch.cells.len() + batch.cells.len() / 2 + 3,
        TextEffect::WordDecode => batch.cells.len() + batch.word_count,
    };
    let step = if config.effect == TextEffect::MatrixTrail {
        config.speed.matrix_line_duration()
    } else {
        config.speed.step_duration()
    };
    step.saturating_mul(u32::try_from(steps).unwrap_or(u32::MAX))
}

fn sample_cell(
    batch: &TextEffectBatch,
    cell: &AnimatedCell,
    config: TextEffectsConfig,
    now: Instant,
) -> TextEffectSample {
    let elapsed = now.saturating_duration_since(batch.started_at);
    let step = config.speed.step_duration();
    let frame = elapsed.as_nanos() / step.as_nanos().max(1);
    let frame = usize::try_from(frame).unwrap_or(usize::MAX);
    let cell_count = batch.cells.len();

    let mut sample = match config.effect {
        TextEffect::Immediate => TextEffectSample::COMPLETE,
        TextEffect::Typewriter => reveal_sample(frame, cell.ordinal, cell_count),
        TextEffect::WordReveal => reveal_sample(frame, cell.word_ordinal, batch.word_count),
        TextEffect::Decode => {
            let rank = shuffled_rank(batch.seed, cell.point, cell_count);
            scramble_reveal_sample(
                frame,
                rank,
                cell_count,
                config.intensity.trail_cells(),
                matrix_glyph(batch.seed, cell.point, frame),
            )
        }
        TextEffect::MatrixTrail => {
            let matrix_elapsed = now.saturating_duration_since(batch.matrix_started_at);
            let matrix_step = config.speed.matrix_line_duration();
            let matrix_frame = matrix_elapsed.as_nanos() / matrix_step.as_nanos().max(1);
            let matrix_frame = usize::try_from(matrix_frame).unwrap_or(usize::MAX);
            let line_ordinal = batch.matrix_line_offset + cell.line_ordinal;
            let trail_lines = config.intensity.matrix_trail_lines();
            if matrix_frame >= line_ordinal + trail_lines {
                TextEffectSample::COMPLETE
            } else if line_ordinal <= matrix_frame {
                TextEffectSample {
                    content: VisualContent::Substitute(matrix_glyph(
                        batch.seed,
                        cell.point,
                        matrix_frame,
                    )),
                    foreground: None,
                    needs_repaint: true,
                }
            } else {
                TextEffectSample {
                    content: VisualContent::Hidden,
                    foreground: None,
                    needs_repaint: true,
                }
            }
        }
        TextEffect::WordDecode => {
            let rank = (shuffled_rank(batch.seed, cell.point, cell.word_len)
                + cell.ordinal_in_word)
                % cell.word_len.max(1);
            scramble_reveal_sample(
                frame.saturating_sub(cell.word_ordinal),
                rank,
                cell.word_len,
                config.intensity.trail_cells(),
                matrix_glyph(batch.seed, cell.point, frame),
            )
        }
        TextEffect::CasePulse => {
            if frame < cell_count {
                return reveal_sample(frame, cell.ordinal, cell_count);
            }

            let pulse_frame = frame - cell_count;
            let distance_from_center = cell.ordinal.abs_diff(cell_count / 2);
            if pulse_frame > cell_count / 2 + 2 {
                TextEffectSample::COMPLETE
            } else if distance_from_center == pulse_frame {
                let substitute = original_ascii_case_pulse(&cell.fingerprint);
                TextEffectSample {
                    content: substitute
                        .map(VisualContent::Substitute)
                        .unwrap_or(VisualContent::Original),
                    foreground: None,
                    needs_repaint: true,
                }
            } else {
                TextEffectSample {
                    content: VisualContent::Original,
                    foreground: None,
                    needs_repaint: true,
                }
            }
        }
    };

    if config.color.is_enabled() {
        sample.foreground = sample_foreground(config, cell.ordinal, elapsed);
        sample.needs_repaint |= config.color.is_dynamic();
    }

    sample
}

fn reveal_sample(frame: usize, ordinal: usize, total: usize) -> TextEffectSample {
    if frame >= total {
        TextEffectSample::COMPLETE
    } else {
        TextEffectSample {
            content: if ordinal < frame {
                VisualContent::Original
            } else {
                VisualContent::Hidden
            },
            foreground: None,
            needs_repaint: true,
        }
    }
}

fn scramble_reveal_sample(
    frame: usize,
    ordinal: usize,
    total: usize,
    trail_cells: usize,
    substitute: char,
) -> TextEffectSample {
    if frame >= total {
        TextEffectSample::COMPLETE
    } else if ordinal < frame {
        TextEffectSample {
            content: VisualContent::Original,
            foreground: None,
            needs_repaint: true,
        }
    } else if ordinal < frame + trail_cells {
        TextEffectSample {
            content: VisualContent::Substitute(substitute),
            foreground: None,
            needs_repaint: true,
        }
    } else {
        TextEffectSample {
            content: VisualContent::Hidden,
            foreground: None,
            needs_repaint: true,
        }
    }
}

fn sample_foreground(
    config: TextEffectsConfig,
    ordinal: usize,
    elapsed: Duration,
) -> Option<Color> {
    if let Some(color) = config.color.solid_color() {
        return Some(Color::Named(color.ansi_color(config.color_intensity)));
    }

    let step = config.color_speed.step_duration();
    let frame = elapsed.as_nanos() / step.as_nanos().max(1);
    let frame = usize::try_from(frame).unwrap_or(usize::MAX);

    match config.color {
        TextEffectColor::Terminal => None,
        TextEffectColor::Grey
        | TextEffectColor::Red
        | TextEffectColor::Green
        | TextEffectColor::Yellow
        | TextEffectColor::Blue
        | TextEffectColor::Magenta
        | TextEffectColor::Cyan
        | TextEffectColor::White => {
            unreachable!("solid txtFX colours are returned before the animated colour match")
        }
        TextEffectColor::ClassicCycle => {
            const CLASSIC_COLORS: [TextFxNamedColor; 6] = [
                TextFxNamedColor::Grey,
                TextFxNamedColor::Blue,
                TextFxNamedColor::Cyan,
                TextFxNamedColor::White,
                TextFxNamedColor::Cyan,
                TextFxNamedColor::Blue,
            ];
            let color = CLASSIC_COLORS[(ordinal + frame) % CLASSIC_COLORS.len()];
            Some(Color::Named(color.ansi_color(config.color_intensity)))
        }
        TextEffectColor::Rainbow => {
            let hue = (ordinal.saturating_mul(29) + frame.saturating_mul(7)) % 360;
            Some(Color::Spec(rainbow_color(hue, config.color_intensity)))
        }
    }
}

fn rainbow_color(hue: usize, intensity: TextEffectColorIntensity) -> ColorU {
    let (saturation, value) = intensity.rainbow_levels();
    let sector = (hue % 360) / 60;
    let remainder = (hue % 60) as u16;
    let minimum = value * (255 - saturation) / 255;
    let rising = minimum + (value - minimum) * remainder / 60;
    let falling = value - (value - minimum) * remainder / 60;

    let (red, green, blue) = match sector {
        0 => (value, rising, minimum),
        1 => (falling, value, minimum),
        2 => (minimum, value, rising),
        3 => (minimum, falling, value),
        4 => (rising, minimum, value),
        5 => (value, minimum, falling),
        _ => unreachable!("rainbow hue is normalized to six sectors"),
    };
    ColorU::new(red as u8, green as u8, blue as u8, 255)
}

fn shuffled_rank(seed: u64, point: AbsolutePoint, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    usize::try_from(stable_hash(seed, point, 0) % len as u64).unwrap_or_default()
}

fn matrix_glyph(seed: u64, point: AbsolutePoint, frame: usize) -> char {
    let index = stable_hash(seed, point, frame as u64) % MATRIX_GLYPHS.len() as u64;
    MATRIX_GLYPHS[index as usize] as char
}

fn stable_hash(seed: u64, point: AbsolutePoint, frame: u64) -> u64 {
    let mut value = seed
        ^ point.row.wrapping_mul(0x9E37_79B1_85EB_CA87)
        ^ (point.col as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ frame.wrapping_mul(0x1656_67B1_9E37_79F9);
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn original_ascii_case_pulse(content: &str) -> Option<char> {
    let mut chars = content.chars();
    let original = chars.next()?;
    if chars.next().is_some() || !original.is_ascii_alphabetic() {
        return None;
    }
    Some(if original.is_ascii_uppercase() {
        original.to_ascii_lowercase()
    } else {
        original.to_ascii_uppercase()
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
