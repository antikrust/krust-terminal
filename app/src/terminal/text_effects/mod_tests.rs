use instant::Instant;
use warp_terminal::model::char_or_str::CharOrStr;

use super::*;
use crate::terminal::model::ansi::{Handler as _, Processor, ProcessorInput};
use crate::terminal::model::grid::grid_handler::GridHandler;

fn point(col: usize) -> AbsolutePoint {
    AbsolutePoint { row: 4, col }
}

fn config(effect: TextEffect) -> TextEffectsConfig {
    TextEffectsConfig {
        effect,
        speed: TextEffectSpeed::Normal,
        intensity: TextEffectIntensity::Balanced,
        color: TextEffectColor::Terminal,
        color_speed: TextEffectColorSpeed::Normal,
        color_intensity: TextEffectColorIntensity::Balanced,
        scope: TextEffectScope::OutputOnly,
        large_output: TextEffectLargeOutput::Animate,
    }
}

fn timeline(text: &str, started_at: Instant) -> TextEffectsTimeline {
    let mut timeline = TextEffectsTimeline::default();
    timeline.finish_batch(
        text.chars()
            .enumerate()
            .map(|(col, c)| (point(col), c.to_string())),
        started_at,
    );
    timeline
}

#[test]
fn typewriter_reveals_cells_in_order() {
    let started_at = Instant::now();
    let timeline = timeline("abc", started_at);
    let step = TextEffectSpeed::Normal.step_duration();

    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char('a'),
            config(TextEffect::Typewriter),
            started_at,
        ),
        Some(TextEffectSample {
            content: VisualContent::Hidden,
            foreground: None,
            needs_repaint: true,
        })
    );
    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char('a'),
            config(TextEffect::Typewriter),
            started_at + step,
        ),
        Some(TextEffectSample {
            content: VisualContent::Original,
            foreground: None,
            needs_repaint: true,
        })
    );
    assert_eq!(
        timeline.sample(
            point(2),
            CharOrStr::Char('c'),
            config(TextEffect::Typewriter),
            started_at + step * 3,
        ),
        Some(TextEffectSample::COMPLETE)
    );
}

#[test]
fn overwritten_content_is_not_animated() {
    let started_at = Instant::now();
    let timeline = timeline("a", started_at);

    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char('b'),
            config(TextEffect::Typewriter),
            started_at,
        ),
        None
    );
}

#[test]
fn matrix_frames_are_deterministic() {
    let started_at = Instant::now();
    let timeline = timeline("matrix", started_at);
    let now = started_at + TextEffectSpeed::Normal.step_duration() * 2;
    let first = timeline.sample(
        point(3),
        CharOrStr::Char('r'),
        config(TextEffect::MatrixTrail),
        now,
    );
    let second = timeline.sample(
        point(3),
        CharOrStr::Char('r'),
        config(TextEffect::MatrixTrail),
        now,
    );

    assert_eq!(first, second);
    assert!(matches!(
        first,
        Some(TextEffectSample {
            content: VisualContent::Substitute(_),
            foreground: None,
            needs_repaint: true,
        })
    ));
}

#[test]
fn intensity_changes_matrix_trail_length() {
    let started_at = Instant::now();
    let timeline = timeline("abcdef", started_at);
    let mut subtle = config(TextEffect::MatrixTrail);
    subtle.intensity = TextEffectIntensity::Subtle;
    let mut strong = subtle;
    strong.intensity = TextEffectIntensity::Strong;

    assert_eq!(
        timeline.sample(point(5), CharOrStr::Char('f'), subtle, started_at),
        Some(TextEffectSample {
            content: VisualContent::Hidden,
            foreground: None,
            needs_repaint: true,
        })
    );
    assert!(matches!(
        timeline.sample(point(5), CharOrStr::Char('f'), strong, started_at),
        Some(TextEffectSample {
            content: VisualContent::Substitute(_),
            foreground: None,
            needs_repaint: true,
        })
    ));
}

#[test]
fn animation_and_colour_layers_are_independent() {
    let started_at = Instant::now();
    let timeline = timeline("a", started_at);
    let mut color_only = config(TextEffect::Immediate);
    color_only.color = TextEffectColor::Green;
    color_only.color_intensity = TextEffectColorIntensity::Vivid;

    assert_eq!(
        timeline.sample(point(0), CharOrStr::Char('a'), color_only, started_at),
        Some(TextEffectSample {
            content: VisualContent::Original,
            foreground: Some(Color::Named(NamedColor::BrightGreen)),
            needs_repaint: true,
        })
    );
    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char('a'),
            color_only,
            started_at + COLOR_ONLY_DURATION,
        ),
        Some(TextEffectSample::COMPLETE)
    );
}

#[test]
fn classic_colour_cycle_uses_the_original_txtfx_palette_order() {
    let started_at = Instant::now();
    let timeline = timeline("ab", started_at);
    let mut classic = config(TextEffect::Immediate);
    classic.color = TextEffectColor::ClassicCycle;

    let sample =
        |col, now| timeline.sample(point(col), CharOrStr::Char(['a', 'b'][col]), classic, now);
    assert_eq!(
        sample(0, started_at).and_then(|sample| sample.foreground),
        Some(Color::Named(NamedColor::BrightBlack))
    );
    assert_eq!(
        sample(1, started_at).and_then(|sample| sample.foreground),
        Some(Color::Named(NamedColor::Blue))
    );
    assert_eq!(
        sample(0, started_at + TextEffectColorSpeed::Normal.step_duration())
            .and_then(|sample| sample.foreground),
        Some(Color::Named(NamedColor::Blue))
    );
}

#[test]
fn rainbow_colour_is_deterministic_for_a_frame() {
    let started_at = Instant::now();
    let timeline = timeline("rainbow", started_at);
    let mut rainbow = config(TextEffect::Typewriter);
    rainbow.color = TextEffectColor::Rainbow;
    rainbow.color_intensity = TextEffectColorIntensity::Vivid;
    let now = started_at + TextEffectSpeed::Normal.step_duration();

    let first = timeline.sample(point(0), CharOrStr::Char('r'), rainbow, now);
    let second = timeline.sample(point(0), CharOrStr::Char('r'), rainbow, now);
    assert_eq!(first, second);
    assert!(matches!(
        first,
        Some(TextEffectSample {
            foreground: Some(Color::Spec(_)),
            needs_repaint: true,
            ..
        })
    ));
}

#[test]
fn effect_specific_controls_report_when_they_apply() {
    assert!(!TextEffect::Immediate.uses_speed());
    assert!(TextEffect::Typewriter.uses_speed());
    assert!(!TextEffect::Typewriter.uses_intensity());
    assert!(TextEffect::MatrixTrail.uses_intensity());
    assert!(TextEffectColor::Rainbow.is_dynamic());
    assert!(!TextEffectColor::Green.is_dynamic());
}

#[test]
fn scope_controls_each_terminal_surface() {
    assert!(TextEffectScope::OutputOnly.includes(TextEffectSurface::Output));
    assert!(!TextEffectScope::OutputOnly.includes(TextEffectSurface::Command));
    assert!(TextEffectScope::OutputAndCommands.includes(TextEffectSurface::Command));
    assert!(!TextEffectScope::OutputAndCommands.includes(TextEffectSurface::AlternateScreen));
    assert!(TextEffectScope::AllTerminalScreens.includes(TextEffectSurface::AlternateScreen));
}

#[test]
fn large_output_can_skip_animation() {
    let started_at = Instant::now();
    let mut timeline = TextEffectsTimeline::default();
    timeline.finish_batch(
        (0..1_025).map(|col| (AbsolutePoint { row: 4, col }, "x".to_string())),
        started_at,
    );
    let mut large_output_config = config(TextEffect::Typewriter);
    large_output_config.large_output = TextEffectLargeOutput::KeepInstant;

    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char('x'),
            large_output_config,
            started_at,
        ),
        None
    );
}

#[test]
fn pending_writes_keep_last_occurrence_order() {
    let mut timeline = TextEffectsTimeline::default();
    timeline.record_write(point(0));
    timeline.record_write(point(1));
    timeline.record_write(point(0));

    assert_eq!(timeline.take_pending_writes(), vec![point(1), point(0)]);
    assert_eq!(timeline.take_pending_writes(), Vec::new());
}

#[test]
fn case_pulse_finishes_on_original_content() {
    let started_at = Instant::now();
    let timeline = timeline("Ab", started_at);
    let step = TextEffectSpeed::Normal.step_duration();

    let pulse = timeline.sample(
        point(1),
        CharOrStr::Char('b'),
        config(TextEffect::CasePulse),
        started_at + step * 2,
    );
    assert_eq!(
        pulse,
        Some(TextEffectSample {
            content: VisualContent::Substitute('B'),
            foreground: None,
            needs_repaint: true,
        })
    );

    assert_eq!(
        timeline.sample(
            point(1),
            CharOrStr::Char('b'),
            config(TextEffect::CasePulse),
            started_at + step * 8,
        ),
        Some(TextEffectSample::COMPLETE)
    );
}

#[test]
fn parses_spinner_protocol_with_bounded_values() {
    assert_eq!(
        TextEffectsCommand::parse_osc_params(&[b"spinner", b"start", b"build-1", b"7", b"120"]),
        Some(TextEffectsCommand::SpinnerStart {
            id: "build-1".to_string(),
            style: SpinnerStyle::Bar,
            fps: 60,
        })
    );
    assert_eq!(
        TextEffectsCommand::parse_osc_params(&[b"spinner", b"stop", b"bad id"]),
        None
    );
}

#[test]
fn spinner_has_an_explicit_start_update_stop_lifecycle() {
    let started_at = Instant::now();
    let mut timeline = TextEffectsTimeline::default();
    timeline.apply_command(
        TextEffectsCommand::SpinnerStart {
            id: "build".to_string(),
            style: SpinnerStyle::Line,
            fps: 20,
        },
        point(0),
        started_at,
    );
    timeline.finish_batch([(point(0), " ".to_string())], started_at);

    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char(' '),
            config(TextEffect::Immediate),
            started_at,
        ),
        Some(TextEffectSample {
            content: VisualContent::Substitute('|'),
            foreground: None,
            needs_repaint: true,
        })
    );

    timeline.apply_command(
        TextEffectsCommand::SpinnerUpdate {
            id: "build".to_string(),
            style: SpinnerStyle::Quarters,
            fps: 10,
        },
        point(99),
        started_at,
    );
    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char(' '),
            config(TextEffect::Immediate),
            started_at,
        ),
        Some(TextEffectSample {
            content: VisualContent::Substitute('◓'),
            foreground: None,
            needs_repaint: true,
        })
    );

    timeline.apply_command(
        TextEffectsCommand::SpinnerStop {
            id: "build".to_string(),
        },
        point(99),
        started_at,
    );
    assert_eq!(
        timeline.sample(
            point(0),
            CharOrStr::Char(' '),
            config(TextEffect::Immediate),
            started_at,
        ),
        None
    );
}

#[test]
fn grid_writes_create_a_renderable_batch() {
    let mut grid = GridHandler::new_for_test(2, 8);
    grid.input('x');
    grid.on_finish_byte_processing(&ProcessorInput::new(&[]));

    let cell = grid.row(0).expect("first row should exist")[0].clone();
    let sample = grid.sample_text_effect(
        crate::terminal::model::index::Point::new(0, 0),
        &cell,
        config(TextEffect::MatrixTrail),
        Instant::now(),
    );

    assert!(matches!(
        sample,
        Some(TextEffectSample {
            content: VisualContent::Hidden | VisualContent::Original | VisualContent::Substitute(_),
            foreground: None,
            needs_repaint: true,
        })
    ));
}

#[test]
fn osc_spinner_command_reaches_the_grid_timeline() {
    let mut grid = GridHandler::new_for_test(2, 8);
    let mut processor = Processor::new();
    processor.parse_bytes(
        &mut grid,
        b"\x1b]9281;spinner;start;test;line;20\x1b\\ ",
        &mut Vec::new(),
    );

    let cell = grid.row(0).expect("first row should exist")[0].clone();
    let sample = grid.sample_text_effect(
        crate::terminal::model::index::Point::new(0, 0),
        &cell,
        config(TextEffect::Immediate),
        Instant::now(),
    );

    assert!(matches!(
        sample,
        Some(TextEffectSample {
            content: VisualContent::Substitute('|'),
            foreground: None,
            needs_repaint: true,
        })
    ));
}
