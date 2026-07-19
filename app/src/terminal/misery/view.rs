//! Rendering for a single misery line.
//!
//! A `MiseryLineView` is inserted into the block list as rich content, which is
//! the same mechanism agent blocks and zero-states use. That gets height,
//! scrolling and selection handled for free, and — crucially — keeps the line
//! out of the PTY byte stream. It looks like it's in the output; it isn't. A
//! command's real stdout is untouched, so `ls | wc -l` still counts files
//! rather than insults.

use warpui::elements::{Container, Element, Text};
use warpui::fonts::{Properties, Style};
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext};

use crate::appearance::Appearance;

/// Left inset so the line sits under the command text rather than against the
/// window edge, roughly aligned with block output.
const LEFT_PADDING: f32 = 12.;
const VERTICAL_PADDING: f32 = 2.;

pub struct MiseryLineView {
    line: String,
}

impl MiseryLineView {
    pub fn new(line: String, _ctx: &mut ViewContext<Self>) -> Self {
        Self { line }
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
        .with_padding_top(VERTICAL_PADDING)
        .with_padding_bottom(VERTICAL_PADDING)
        .finish()
    }
}
