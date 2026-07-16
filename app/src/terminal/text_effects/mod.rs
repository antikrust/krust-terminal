mod picker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use instant::Instant;
pub use picker::{TextEffectPicker, TextEffectPickerEvent};
use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{Setting as _, SupportedPlatforms, SyncToCloud};
use warp_terminal::model::char_or_str::CharOrStr;
use warpui::{AppContext, SingletonEntity};

use crate::features::FeatureFlag;
use crate::terminal::model::grid::grid_handler::AbsolutePoint;

const MAX_PENDING_WRITES: usize = 4_096;
const MAX_ACTIVE_BATCHES: usize = 8;
const MATRIX_GLYPHS: &[u8] = b"!@#$%^&*+-=<>?/\\|:;";
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

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpinnerStyle {
    #[default]
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
    pub const VALUES: [Self; 8] = [
        Self::Line,
        Self::Orbit,
        Self::Arrows,
        Self::Quarters,
        Self::Fill,
        Self::Clock,
        Self::Bar,
        Self::Bounce,
    ];

    pub fn frames(self) -> &'static str {
        match self {
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
    pub intensity: TextEffectIntensity,
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
            intensity: *settings.intensity.value(),
            scope: *settings.scope.value(),
            large_output: *settings.large_output.value(),
        })
    }

    pub fn for_surface(app: &AppContext, surface: TextEffectSurface) -> Option<Self> {
        Self::from_app(app).filter(|config| config.scope.includes(surface))
    }

    pub fn frame_interval(self) -> Duration {
        self.speed.frame_interval()
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
    pub needs_repaint: bool,
}

impl TextEffectSample {
    const COMPLETE: Self = Self {
        content: VisualContent::Original,
        needs_repaint: false,
    };
}

#[derive(Clone, Debug)]
struct AnimatedCell {
    point: AbsolutePoint,
    fingerprint: String,
    ordinal: usize,
    word_ordinal: usize,
    ordinal_in_word: usize,
    word_len: usize,
}

#[derive(Clone, Debug)]
struct TextEffectBatch {
    started_at: Instant,
    seed: u64,
    cells: Vec<AnimatedCell>,
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
}

impl TextEffectsTimeline {
    pub fn clear(&mut self) {
        self.pending_writes.clear();
        self.batches.clear();
        self.spinners.clear();
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

        for (_, content) in &written_cells {
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
            .enumerate()
            .map(
                |(ordinal, ((point, fingerprint), (word_ordinal, ordinal_in_word)))| {
                    let word_len = word_lengths.get(word_ordinal).copied().unwrap_or_default();
                    AnimatedCell {
                        point,
                        fingerprint,
                        ordinal,
                        word_ordinal,
                        ordinal_in_word,
                        word_len,
                    }
                },
            )
            .collect();

        self.next_seed = self.next_seed.wrapping_add(1);
        self.batches.push_back(TextEffectBatch {
            started_at,
            seed: self.next_seed,
            cells,
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
                needs_repaint: true,
            });
        }

        if config.effect == TextEffect::Immediate {
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

fn content_matches(fingerprint: &str, content: CharOrStr<'_>) -> bool {
    match content {
        CharOrStr::Char(character) => {
            let mut buffer = [0; 4];
            fingerprint == character.encode_utf8(&mut buffer)
        }
        CharOrStr::Str(content) => fingerprint == content,
    }
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

    match config.effect {
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
            let trail_cells = config.intensity.trail_cells();
            if frame >= cell_count + trail_cells {
                TextEffectSample::COMPLETE
            } else if cell.ordinal < frame {
                TextEffectSample {
                    content: VisualContent::Original,
                    needs_repaint: true,
                }
            } else if cell.ordinal < frame + trail_cells {
                TextEffectSample {
                    content: VisualContent::Substitute(matrix_glyph(batch.seed, cell.point, frame)),
                    needs_repaint: true,
                }
            } else {
                TextEffectSample {
                    content: VisualContent::Hidden,
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
                    needs_repaint: true,
                }
            } else {
                TextEffectSample {
                    content: VisualContent::Original,
                    needs_repaint: true,
                }
            }
        }
    }
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
            needs_repaint: true,
        }
    } else if ordinal < frame + trail_cells {
        TextEffectSample {
            content: VisualContent::Substitute(substitute),
            needs_repaint: true,
        }
    } else {
        TextEffectSample {
            content: VisualContent::Hidden,
            needs_repaint: true,
        }
    }
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
