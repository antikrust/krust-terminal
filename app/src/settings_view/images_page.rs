//! Settings → AI → Images: where pictures are saved, the picture suppliers (a key each, with a link
//! to where to get one) and the failover list that orders them. `generate_image` and `imagine`
//! (see `ai::image_gen`) try the local command first, then these models in this order.

use std::cell::RefCell;

use krust_errors::report_if_error;
use krustui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Flex, MouseStateHandle, ParentElement, Text,
};
use krustui::ui_components::button::ButtonVariant;
use krustui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use krustui::ui_components::switch::SwitchStateHandle;
use krustui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};
use settings::Setting as _;

use crate::ai::api_providers::ApiProviderKeys;
use crate::ai::image_providers::{self, IMAGE_PROVIDERS};
use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::settings::AISettings;

use super::settings_page::{
    MatchData, PageTitleWidget, PageType, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
};
use super::SettingsSection;

const VERTICAL_MARGIN: f32 = 16.;

pub struct ImagesPageView {
    page: PageType<Self>,
    folder_editor: ViewHandle<EditorView>,
    /// One key field per supplier, by supplier id.
    key_editors: Vec<(&'static str, ViewHandle<EditorView>)>,
    handles: RefCell<Handles>,
    message: Option<String>,
}

#[derive(Default)]
struct Handles {
    folder_save: MouseStateHandle,
    /// Per supplier, in the order of `IMAGE_PROVIDERS`: its "Get a key" link and Save button.
    key_links: Vec<MouseStateHandle>,
    key_saves: Vec<MouseStateHandle>,
    /// Per failover row, by model id: its switch and its up and down buttons.
    rows: Vec<(String, SwitchStateHandle, MouseStateHandle, MouseStateHandle)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImagesPageAction {
    SaveFolder,
    SaveKey(&'static str),
    OpenKeyPage(&'static str),
    ToggleRow(String),
    MoveRow(String, isize),
}

impl ImagesPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(PageTitleWidget::new("Images", true)),
            Box::new(ImagesWidget),
        ];
        let folder = AISettings::as_ref(ctx)
            .image_output_dir
            .value()
            .clone()
            .unwrap_or_default();
        let folder_editor = Self::field("Folder for pictures (default: image-lab/images)", &folder, false, ctx, |me, ctx| {
            me.save_folder(ctx)
        });
        let key_editors = IMAGE_PROVIDERS
            .iter()
            .map(|provider| {
                let saved = ApiProviderKeys::as_ref(ctx)
                    .key_for(provider.id)
                    .unwrap_or_default()
                    .to_owned();
                let id = provider.id;
                let editor = Self::field("API key", &saved, true, ctx, move |me, ctx| {
                    me.save_key(id, ctx)
                });
                (id, editor)
            })
            .collect();
        let handles = Handles {
            key_links: IMAGE_PROVIDERS.iter().map(|_| MouseStateHandle::default()).collect(),
            key_saves: IMAGE_PROVIDERS.iter().map(|_| MouseStateHandle::default()).collect(),
            ..Default::default()
        };
        ctx.subscribe_to_model(&ApiProviderKeys::handle(ctx), |_, _, _, ctx| ctx.notify());
        ctx.subscribe_to_model(&AISettings::handle(ctx), |_, _, _, ctx| ctx.notify());
        Self {
            page: PageType::new_uncategorized(widgets, None),
            folder_editor,
            key_editors,
            handles: RefCell::new(handles),
            message: None,
        }
    }

    /// A single-line field; Enter runs `on_enter`.
    fn field(
        placeholder: &'static str,
        text: &str,
        is_password: bool,
        ctx: &mut ViewContext<Self>,
        on_enter: impl Fn(&mut Self, &mut ViewContext<Self>) + 'static,
    ) -> ViewHandle<EditorView> {
        let text = text.to_owned();
        let editor = ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::handle(ctx).as_ref(ctx);
            let options = SingleLineEditorOptions {
                is_password,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    text_colors_override: Some(TextColors {
                        default_color: appearance.theme().active_ui_text_color(),
                        disabled_color: appearance.theme().disabled_ui_text_color(),
                        hint_color: appearance.theme().disabled_ui_text_color(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(placeholder, ctx);
            if !text.is_empty() {
                editor.set_buffer_text(&text, ctx);
            }
            editor
        });
        ctx.subscribe_to_view(&editor, move |me, _, event, ctx| {
            if matches!(event, EditorEvent::Enter) {
                on_enter(me, ctx);
            }
        });
        editor
    }

    fn save_folder(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.folder_editor.as_ref(ctx).buffer_text(ctx).trim().to_owned();
        let folder = (!text.is_empty()).then_some(text);
        AISettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.image_output_dir.set_value(folder, ctx));
        });
        self.message = Some("Saved the folder for pictures.".to_owned());
        ctx.notify();
    }

    fn save_key(&mut self, provider_id: &'static str, ctx: &mut ViewContext<Self>) {
        let Some(text) = self
            .key_editors
            .iter()
            .find(|(id, _)| *id == provider_id)
            .map(|(_, editor)| editor.as_ref(ctx).buffer_text(ctx).trim().to_owned())
        else {
            return;
        };
        let key = (!text.is_empty()).then_some(text);
        let saved = key.is_some();
        ApiProviderKeys::handle(ctx).update(ctx, |keys, ctx| keys.set_key(provider_id, key, ctx));
        let name = image_providers::provider(provider_id).map_or(provider_id, |p| p.display_name);
        self.message = Some(if saved {
            format!("Saved the {name} key.")
        } else {
            format!("Removed the {name} key.")
        });
        ctx.notify();
    }

    /// A failover row's switch and its up and down buttons, made once and kept.
    fn row_state(&self, id: &str) -> (SwitchStateHandle, MouseStateHandle, MouseStateHandle) {
        let mut handles = self.handles.borrow_mut();
        if let Some((_, switch, up, down)) = handles.rows.iter().find(|(row, ..)| row == id) {
            return (switch.clone(), up.clone(), down.clone());
        }
        let made = (
            id.to_owned(),
            SwitchStateHandle::default(),
            MouseStateHandle::default(),
            MouseStateHandle::default(),
        );
        handles.rows.push(made.clone());
        (made.1, made.2, made.3)
    }
}

impl Entity for ImagesPageView {
    type Event = ();
}

impl View for ImagesPageView {
    fn ui_name() -> &'static str {
        "ImagesPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl TypedActionView for ImagesPageView {
    type Action = ImagesPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ImagesPageAction::SaveFolder => self.save_folder(ctx),
            ImagesPageAction::SaveKey(provider_id) => self.save_key(*provider_id, ctx),
            ImagesPageAction::OpenKeyPage(provider_id) => {
                if let Some(provider) = image_providers::provider(provider_id) {
                    ctx.open_url(provider.key_url);
                }
            }
            ImagesPageAction::ToggleRow(id) => {
                let mut disabled = AISettings::as_ref(ctx).image_failover_disabled.value().clone();
                if let Some(at) = disabled.iter().position(|row| row == id) {
                    disabled.remove(at);
                } else {
                    disabled.push(id.clone());
                }
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.image_failover_disabled.set_value(disabled, ctx));
                });
                ctx.notify();
            }
            ImagesPageAction::MoveRow(id, delta) => {
                let rows = image_providers::failover_rows(ctx);
                if let Some(index) = rows.iter().position(|row| row == id) {
                    let order = image_providers::moved(&rows, index, *delta);
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        report_if_error!(settings.image_failover_order.set_value(order, ctx));
                    });
                }
                ctx.notify();
            }
        }
    }
}

impl SettingsPageMeta for ImagesPageView {
    fn section() -> SettingsSection {
        SettingsSection::ImageProviders
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        cfg!(not(target_family = "wasm"))
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<ImagesPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<ImagesPageView>) -> Self {
        SettingsPageViewHandle::Images(view_handle)
    }
}

struct ImagesWidget;

impl SettingsWidget for ImagesWidget {
    type View = ImagesPageView;

    fn search_terms(&self) -> &str {
        "images pictures image generation imagine suppliers keys openai together xai failover folder"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let text = |label: String| {
            Text::new_inline(label, appearance.ui_font_family(), 13.)
                .with_color(theme.active_ui_text_color().into())
                .finish()
        };
        let note = |label: &str| {
            Text::new(
                label.to_string(),
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_color(theme.sub_text_color(theme.background()).into_solid())
            .finish()
        };
        let input = |editor: &ViewHandle<EditorView>| {
            ConstrainedBox::new(
                appearance
                    .ui_builder()
                    .text_input(editor.clone())
                    .with_style(UiComponentStyles {
                        padding: Some(Coords {
                            top: 6.,
                            bottom: 6.,
                            left: 10.,
                            right: 10.,
                        }),
                        background: Some(theme.surface_2().into()),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            )
            .with_width(360.)
            .finish()
        };
        let handles = view.handles.borrow();

        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(8.);
        column.add_child(note(
            "Pictures for the generate_image tool and the imagine command. They try the local \
             command first (the image_command setting), then the models below in this order, then \
             any agent model that makes images. Keys are kept with the other API keys.",
        ));

        // Where pictures go when no file is named.
        column.add_child(text("Folder for pictures".to_owned()));
        column.add_child(
            Flex::row()
                .with_spacing(8.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(input(&view.folder_editor))
                .with_child(
                    appearance
                        .ui_builder()
                        .button(ButtonVariant::Secondary, handles.folder_save.clone())
                        .with_text_label("Save".to_owned())
                        .build()
                        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ImagesPageAction::SaveFolder))
                        .finish(),
                )
                .finish(),
        );

        // The suppliers.
        for (index, provider) in IMAGE_PROVIDERS.iter().enumerate() {
            let Some((_, editor)) = view.key_editors.iter().find(|(id, _)| *id == provider.id) else {
                continue;
            };
            let id = provider.id;
            column.add_child(
                Container::new(text(provider.display_name.to_owned()))
                    .with_margin_top(VERTICAL_MARGIN / 2.)
                    .finish(),
            );
            column.add_child(note(provider.note));
            column.add_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(input(editor))
                    .with_child(
                        appearance
                            .ui_builder()
                            .button(ButtonVariant::Secondary, handles.key_saves[index].clone())
                            .with_text_label("Save".to_owned())
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(ImagesPageAction::SaveKey(id))
                            })
                            .finish(),
                    )
                    .with_child(
                        appearance
                            .ui_builder()
                            .button(ButtonVariant::Text, handles.key_links[index].clone())
                            .with_text_label("Get a key".to_owned())
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(ImagesPageAction::OpenKeyPage(id))
                            })
                            .finish(),
                    )
                    .finish(),
            );
        }
        drop(handles);

        // The failover list.
        column.add_child(
            Container::new(text("Order tried (a supplier with a key)".to_owned()))
                .with_margin_top(VERTICAL_MARGIN)
                .finish(),
        );
        let rows = image_providers::failover_rows(app);
        if rows.is_empty() {
            column.add_child(note("No supplier has a key yet: add one above."));
        }
        for id in rows {
            let (switch_state, up_state, down_state) = view.row_state(&id);
            let on = !image_providers::is_disabled(&id, app);
            let (toggle_id, up_id, down_id) = (id.clone(), id.clone(), id.clone());
            column.add_child(
                Flex::row()
                    .with_spacing(10.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        appearance
                            .ui_builder()
                            .switch(switch_state)
                            .check(on)
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(ImagesPageAction::ToggleRow(
                                    toggle_id.clone(),
                                ))
                            })
                            .finish(),
                    )
                    .with_child(text(image_providers::label(&id)))
                    .with_child(
                        appearance
                            .ui_builder()
                            .button(ButtonVariant::Text, up_state)
                            .with_text_label("up".to_owned())
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(ImagesPageAction::MoveRow(
                                    up_id.clone(),
                                    -1,
                                ))
                            })
                            .finish(),
                    )
                    .with_child(
                        appearance
                            .ui_builder()
                            .button(ButtonVariant::Text, down_state)
                            .with_text_label("down".to_owned())
                            .build()
                            .on_click(move |ctx, _, _| {
                                ctx.dispatch_typed_action(ImagesPageAction::MoveRow(
                                    down_id.clone(),
                                    1,
                                ))
                            })
                            .finish(),
                    )
                    .finish(),
            );
        }
        if let Some(message) = &view.message {
            column.add_child(note(message));
        }
        Container::new(column.finish())
            .with_margin_bottom(VERTICAL_MARGIN)
            .finish()
    }
}
