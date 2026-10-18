//! Rendering for a single misery line.
//!
//! A `MiseryLineView` is inserted into the block list as rich content, which is
//! the same mechanism agent blocks and zero-states use. That gets height,
//! scrolling and selection handled for free, and — crucially — keeps the line
//! out of the PTY byte stream. It looks like it's in the output; it isn't. A
//! command's real stdout is untouched, so `ls | wc -l` still counts files
//! rather than insults.

use krustui::elements::{Container, Element, Text};
use krustui::fonts::{Properties, Style};
use krustui::{AppContext, Entity, SingletonEntity, View, ViewContext};

use super::Phase;
use crate::appearance::Appearance;

/// Left inset so the line sits under the command text rather than against the
/// window edge, roughly aligned with block output.
const LEFT_PADDING: f32 = 12.;
/// Gap on the side the line is *not* attached to.
const DETACHED_PADDING: f32 = 10.;
/// Gap on the side the line belongs to. Kept tight so proximity groups it with
/// the right block.
const ATTACHED_PADDING: f32 = 1.;

pub struct MiseryLineView {
    line: String,
    phase: Phase,
}

impl MiseryLineView {
    pub fn new(line: String, phase: Phase, _ctx: &mut ViewContext<Self>) -> Self {
        Self { line, phase }
    }
}

impl Entity for MiseryLineView {
    type Event = ();
}

impl View for MiseryLineView {
    fn ui_name() -> &'static str {
        "MiseryLine"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        // Submit lines precede their command, so they hug what follows;
        // completion lines follow theirs, so they hug what precedes.
        let (top_padding, bottom_padding) = match self.phase {
            Phase::Submit => (DETACHED_PADDING, ATTACHED_PADDING),
            Phase::Complete => (ATTACHED_PADDING, DETACHED_PADDING),
            // The quote reacts to a prompt the user just sent the agent; it
            // stands on its own, detached from the blocks either side.
            Phase::Quote => (DETACHED_PADDING, DETACHED_PADDING),
        };

        // Dim and italic: the terminal is muttering, not announcing. Using the
        // sub-text colour keeps it clearly subordinate to real output, which
        // matters because this is the one piece of text on screen that no
        // program actually printed.
        Container::new(
            Text::new(
                self.line.clone(),
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_style(Properties::default().style(Style::Italic))
            .with_color(theme.sub_text_color(theme.background()).into_solid())
            .finish(),
        )
        .with_padding_left(LEFT_PADDING)
        // Asymmetric spacing is what makes the two voices legible. Both lines
        // sit between blocks, so without this a submit line is
        // indistinguishable from a verdict on the block above it. Hugging the
        // block it refers to lets proximity do the disambiguating.
        .with_padding_top(top_padding)
        .with_padding_bottom(bottom_padding)
        .finish()
    }
}
