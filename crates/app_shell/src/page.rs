//! The pieces a page is made of, in Krust's settings-page style: a title with its links, label and
//! value rows, dim notes, monospace lines, small link buttons, and a body that scrolls.

use krust_core::ui::appearance::Appearance;
use krustui::Action;
use krustui::elements::{
    ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CrossAxisAlignment,
    Element, EventContext, Expanded, Fill, Flex, MouseStateHandle, ParentElement, ScrollbarWidth,
    Text,
};
use krustui::fonts::{Properties, Weight};
use krustui::ui_components::components::UiComponent;

/// Space around a page, and between its blocks.
pub const PAGE_PADDING: f32 = 24.;
pub const BLOCK_GAP: f32 = 16.;
/// The width labels take in a row.
const LABEL_WIDTH: f32 = 130.;

/// A page: the title (with its links on the right) over `body`, which scrolls.
pub fn frame(
    title: &str,
    links: Vec<Box<dyn Element>>,
    body: Vec<Box<dyn Element>>,
    scroll: &ClippedScrollStateHandle,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut header = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            Text::new_inline(title.to_owned(), appearance.ui_font_family(), 18.)
                .with_color(theme.active_ui_text_color().into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .with_child(Expanded::new(1., Flex::row().finish()).finish());
    let mut link_row = Flex::row().with_spacing(16.);
    for link in links {
        link_row.add_child(link);
    }
    header.add_child(link_row.finish());

    let mut column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(BLOCK_GAP)
        .with_child(header.finish());
    for block in body {
        column.add_child(block);
    }
    ClippedScrollable::vertical(
        scroll.clone(),
        Container::new(column.finish())
            .with_uniform_padding(PAGE_PADDING)
            .finish(),
        ScrollbarWidth::Auto,
        theme.nonactive_ui_detail().into(),
        theme.active_ui_detail().into(),
        Fill::None,
    )
    .finish()
}

/// `label  value`, the label dim.
pub fn row(label: &str, value: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_child(
            ConstrainedBox::new(
                Text::new_inline(label.to_owned(), appearance.ui_font_family(), 13.)
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
            )
            .with_width(LABEL_WIDTH)
            .finish(),
        )
        .with_child(
            Expanded::new(
                1.,
                appearance
                    .ui_builder()
                    .wrappable_text(value.to_owned(), true)
                    .build()
                    .finish(),
            )
            .finish(),
        )
        .finish()
}

/// A labelled control (a field, a switch), lined up with the rows.
pub fn labelled(label: &str, control: Box<dyn Element>, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_child(
            ConstrainedBox::new(
                Text::new_inline(label.to_owned(), appearance.ui_font_family(), 13.)
                    .with_color(theme.active_ui_text_color().into())
                    .finish(),
            )
            .with_width(LABEL_WIDTH)
            .finish(),
        )
        .with_child(control)
        .finish()
}

/// A dim note.
pub fn note(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new(text.to_owned(), appearance.ui_font_family(), 12.)
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish()
}

/// A block's heading.
pub fn heading(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    Text::new_inline(text.to_owned(), appearance.ui_font_family(), 14.)
        .with_color(theme.active_ui_text_color().into())
        .with_style(Properties::default().weight(Weight::Semibold))
        .finish()
}

/// Lines of log or other output, in Krust's monospace font.
pub fn mono_lines(lines: &[String], appearance: &Appearance) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut column = Flex::column().with_spacing(2.);
    for line in lines {
        column.add_child(
            Text::new(
                line.clone(),
                appearance.monospace_font_family(),
                appearance.monospace_font_size() - 1.,
            )
            .with_color(theme.main_text_color(theme.background()).into())
            .finish(),
        );
    }
    column.finish()
}

/// A small blue link that dispatches `action` when clicked.
pub fn link<A: Action + Clone>(
    label: &str,
    action: A,
    mouse: &MouseStateHandle,
    appearance: &Appearance,
) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .link(
            label.to_owned(),
            None,
            Some(Box::new(move |ctx: &mut EventContext| {
                ctx.dispatch_typed_action(action.clone());
            })),
            mouse.clone(),
        )
        .soft_wrap(false)
        .build()
        .finish()
}
