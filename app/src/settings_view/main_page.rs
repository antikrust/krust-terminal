use std::sync::{Arc, Mutex};

use ::settings::{Setting, ToggleableSetting};
use lazy_static::lazy_static;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use bork_core::channel::ChannelState;
use bork_core::context_flag::ContextFlag;
use bork_core::features::FeatureFlag;
use bork_core::ui::icons::Icon;
use bork_errors::{report_error, report_if_error};
#[cfg(not(target_family = "wasm"))]
use bork_server_client::iap::{IapCredentialsState, IapManager, IapManagerEvent};
use borkui::assets::asset_cache::AssetSource;
use borkui::elements::{
    Align, Border, CacheOption, ChildView, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, Element, Empty, Expanded, Flex, Hoverable, Image, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, ParentElement, Radius, Shrinkable, Text,
};
use borkui::fonts::Weight;
use borkui::keymap::ContextPredicate;
use borkui::platform::Cursor;
use borkui::ui_components::button::{ButtonVariant, TextAndIcon, TextAndIconAlignment};
use borkui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use borkui::ui_components::switch::SwitchStateHandle;
use borkui::{
    id, Action, AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

use super::avatar_picker::{AvatarPickerModal, AVATAR_MODAL_WIDTH};
use super::billing_and_usage_dispatch::BillingAndUsageDispatchView;
use super::billing_and_usage_page::BillingAndUsagePageEvent;
use super::settings_page::{
    render_body_item, render_customer_type_badge, render_sub_header, AdditionalInfo,
    LocalOnlyIconState, MatchData, PageType, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, ToggleState, HEADER_FONT_SIZE, HEADER_PADDING,
};
use super::teams_page::{TeamsPageView, TeamsPageViewEvent};
use super::{flags, SettingsAction, SettingsSection, ToggleSettingActionPair};
use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, LoginGatedFeature};
use crate::auth::auth_state::AuthState;
use crate::auth::auth_view_modal::AuthViewVariant;
use crate::auth::{AuthStateProvider, UserUid};
use crate::autoupdate::{self, AutoupdateStage, AutoupdateState};
use crate::modal::{Modal, ModalEvent, ModalViewState};
use crate::server::ids::ServerId;
use crate::settings::cloud_preferences::CloudPreferencesSettings;
use crate::settings::identity::IdentitySettings;
use crate::workspace::WorkspaceAction;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::workspaces::workspace::CustomerType;
use crate::{send_telemetry_from_ctx, TelemetryEvent};

/// The one avatar on the page — bigger than the old row of choices, since it
/// is now also the button that opens the picker.
const AVATAR_DISPLAY_SIZE: f32 = 56.;
const REGULAR_TEXT_FONT_SIZE: f32 = 12.;
const VERTICAL_MARGIN: f32 = 24.;
const LOG_OUT_TEXT: &str = "Log out";
lazy_static! {
    static ref SETTINGS_SYNC_BINDINGS_ADDED: Arc<Mutex<bool>> = Default::default();
}

pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    let mut toggle_binding_pairs = Vec::new();
    maybe_add_settings_sync_toggle_binding(app, context, builder, &mut toggle_binding_pairs);

    // Add other bindings here in the future.

    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(toggle_binding_pairs, app);
}

fn maybe_add_settings_sync_toggle_binding<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
    toggle_binding_pairs: &mut Vec<ToggleSettingActionPair<T>>,
) {
    let mut lock = SETTINGS_SYNC_BINDINGS_ADDED
        .lock()
        .expect("settings sync bindings lock poisoned");
    if !*lock {
        *lock = true;
        toggle_binding_pairs.push(
            ToggleSettingActionPair::new(
                "settings sync",
                builder(SettingsAction::MainPageToggle(
                    MainPageAction::ToggleSettingsSync,
                )),
                context,
                flags::SETTINGS_SYNC_FLAG,
            )
            .is_supported_on_current_platform(
                CloudPreferencesSettings::as_ref(app)
                    .settings_sync_enabled
                    .is_supported_on_current_platform(),
            ),
        );
    }
}

pub fn handle_experiment_change(app: &mut AppContext) {
    let mut toggle_binding_pairs: Vec<ToggleSettingActionPair<WorkspaceAction>> = Vec::new();
    maybe_add_settings_sync_toggle_binding(
        app,
        &id!("Workspace"),
        WorkspaceAction::DispatchToSettingsTab,
        &mut toggle_binding_pairs,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(toggle_binding_pairs, app);
}

#[derive(Debug, Clone)]
pub enum MainPageAction {
    Relaunch,
    DownloadUpdate,
    CheckForUpdate,
    ToggleSettingsSync,
    Upgrade {
        team_uid: Option<ServerId>,
        user_id: UserUid,
    },
    GenerateStripeBillingPortalLink {
        team_uid: ServerId,
    },
    SignupAnonymousUser,
    OpenUrl(String),
    #[cfg(not(target_family = "wasm"))]
    RefreshIapCredentials,
    /// Open the avatar picker modal (see [`super::avatar_picker`]). All avatar
    /// changes happen inside the modal, which writes straight to settings.
    OpenAvatarPicker,
}

impl MainPageAction {
    fn blocked_for_anonymous_user(&self) -> bool {
        use MainPageAction::*;
        matches!(
            self,
            Upgrade { .. } | GenerateStripeBillingPortalLink { .. } | ToggleSettingsSync,
        )
    }
}

impl From<&MainPageAction> for LoginGatedFeature {
    fn from(val: &MainPageAction) -> LoginGatedFeature {
        use MainPageAction::*;
        match val {
            Upgrade { .. } => "Upgrade Plan",
            GenerateStripeBillingPortalLink { .. } => "Generate Stripe Billing Portal Link",
            ToggleSettingsSync => "Toggle Settings Sync",
            _ => "Unknown reason",
        }
    }
}

#[derive(Clone)]
pub enum MainSettingsPageEvent {
    CheckForUpdate,
    #[allow(dead_code)]
    OpenBorkDrive,
    SignupAnonymousUser,
    /// Re-emitted from the embedded billing & usage section.
    Billing(BillingAndUsagePageEvent),
    /// Re-emitted from the embedded teams section.
    Teams(TeamsPageViewEvent),
    /// The avatar picker modal opened or closed. The modal element is rendered
    /// by the parent settings view (see `get_modal_content_for_page`), so the
    /// parent must repaint on toggles it cannot otherwise see.
    AvatarPickerToggled,
}

pub struct MainSettingsPageView {
    page: PageType<Self>,
    auth_state: Arc<AuthState>,
    /// Billing & usage used to be its own settings page; it now renders as the
    /// last section of this page.
    billing_and_usage: ViewHandle<BillingAndUsageDispatchView>,
    /// Teams likewise: the full team-management UI, hosted here rather than on
    /// a page of its own.
    teams: ViewHandle<TeamsPageView>,
    /// The avatar picker modal, opened by clicking the avatar in the account
    /// header. The body view holds only browsing state; choices go straight to
    /// [`IdentitySettings`].
    avatar_picker_modal: ModalViewState<Modal<AvatarPickerModal>>,
    avatar_picker_body: ViewHandle<AvatarPickerModal>,
}

impl Entity for MainSettingsPageView {
    type Event = MainSettingsPageEvent;
}

impl TypedActionView for MainSettingsPageView {
    type Action = MainPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        // Block anonymous users from upgrading
        if AuthStateProvider::as_ref(ctx)
            .get()
            .is_anonymous_or_logged_out()
            && action.blocked_for_anonymous_user()
        {
            AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                auth_manager.attempt_login_gated_feature(
                    action.into(),
                    AuthViewVariant::RequireLoginCloseable,
                    ctx,
                )
            });
            return;
        }

        match action {
            MainPageAction::OpenAvatarPicker => {
                self.avatar_picker_body
                    .update(ctx, |body, ctx| body.sync_to_current_choice(ctx));
                self.avatar_picker_modal.open();
                ctx.emit(MainSettingsPageEvent::AvatarPickerToggled);
                ctx.notify();
            }
            MainPageAction::Relaunch => {
                autoupdate::initiate_relaunch_for_update(ctx);
            }
            MainPageAction::DownloadUpdate => {
                autoupdate::manually_download_new_version(ctx);
            }
            MainPageAction::CheckForUpdate => {
                ctx.emit(MainSettingsPageEvent::CheckForUpdate);
                ctx.notify();
            }
            MainPageAction::ToggleSettingsSync => {
                let new_value =
                    CloudPreferencesSettings::handle(ctx).update(ctx, |prefs_settings, ctx| {
                        report_if_error!(prefs_settings
                            .settings_sync_enabled
                            .toggle_and_save_value(ctx));
                        *prefs_settings.settings_sync_enabled
                    });
                send_telemetry_from_ctx!(
                    TelemetryEvent::ToggleSettingsSync {
                        is_settings_sync_enabled: new_value,
                    },
                    ctx
                );
                ctx.notify();
            }
            MainPageAction::Upgrade { team_uid, user_id } => match team_uid {
                Some(team_uid) => {
                    ctx.open_url(&UserWorkspaces::upgrade_link_for_team(*team_uid));
                }
                None => {
                    ctx.open_url(&UserWorkspaces::upgrade_link(*user_id));
                }
            },
            MainPageAction::GenerateStripeBillingPortalLink { team_uid } => {
                UserWorkspaces::handle(ctx).update(ctx, |user_workspaces, ctx| {
                    user_workspaces.generate_stripe_billing_portal_link(*team_uid, ctx);
                });
            }
            MainPageAction::SignupAnonymousUser => {
                ctx.emit(MainSettingsPageEvent::SignupAnonymousUser);
            }
            MainPageAction::OpenUrl(url) => {
                ctx.open_url(url);
            }
            #[cfg(not(target_family = "wasm"))]
            MainPageAction::RefreshIapCredentials => {
                IapManager::handle(ctx).update(ctx, |manager, ctx| manager.start_refresh(ctx));
                ctx.notify();
            }
        }
    }
}

impl View for MainSettingsPageView {
    fn ui_name() -> &'static str {
        "MainSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl MainSettingsPageView {
    pub fn new(ctx: &mut ViewContext<MainSettingsPageView>) -> Self {
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();

        let autoupdate_state_handle = AutoupdateState::handle(ctx);
        ctx.observe(
            &autoupdate_state_handle,
            Self::handle_autoupdate_state_change,
        );

        ctx.subscribe_to_model(&CloudPreferencesSettings::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });

        let auth_manager_handle = AuthManager::handle(ctx);
        ctx.subscribe_to_model(&auth_manager_handle, |_, _, _, ctx| {
            ctx.notify();
        });

        // The avatar (shown in the account header, chosen via a modal) is
        // local identity, not account data, so it works signed out. The page
        // repaints on identity changes because the modal writes settings
        // directly rather than dispatching page actions.
        ctx.subscribe_to_model(&IdentitySettings::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });
        let avatar_picker_body = ctx.add_typed_action_view(AvatarPickerModal::new);
        let avatar_picker_modal_view = {
            let body = avatar_picker_body.clone();
            ctx.add_typed_action_view(move |ctx| {
                Modal::new(Some("Choose your avatar".to_string()), body, ctx)
                    .with_modal_style(UiComponentStyles {
                        width: Some(AVATAR_MODAL_WIDTH),
                        height: Some(600.),
                        ..Default::default()
                    })
                    // The title matches the settings page headings (same font
                    // family via the modal's appearance handling; same size
                    // and weight here).
                    .with_header_style(UiComponentStyles {
                        font_size: Some(HEADER_FONT_SIZE),
                        font_weight: Some(Weight::Bold),
                        ..Default::default()
                    })
                    .with_body_style(UiComponentStyles {
                        height: Some(530.),
                        ..Default::default()
                    })
                    .with_background_opacity(100)
                    .with_dismiss_on_click()
            })
        };
        ctx.subscribe_to_view(&avatar_picker_modal_view, |me, _, event, ctx| match event {
            ModalEvent::Close => {
                me.avatar_picker_modal.close();
                ctx.emit(MainSettingsPageEvent::AvatarPickerToggled);
                ctx.notify();
            }
        });
        let avatar_picker_modal = ModalViewState::new(avatar_picker_modal_view);

        let mut widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(SignupBannerWidget),
            Box::new(AccountWidget::default()),
            Box::new(DividerWidget {}),
        ];

        widgets.push(Box::new(SettingsSyncWidget::default()));

        #[cfg(not(target_family = "wasm"))]
        if IapManager::as_ref(ctx).is_enabled() {
            widgets.push(Box::new(IapCredentialsWidget::default()));
            let iap_manager_handle = IapManager::handle(ctx);
            ctx.subscribe_to_model(&iap_manager_handle, |_, _, e, ctx| {
                if matches!(e, IapManagerEvent::StateChanged) {
                    ctx.notify();
                }
            })
        }

        if ChannelState::app_version().is_some() {
            widgets.push(Box::new(VersionInfoWidget::default()));
        }

        // Teams and Billing & usage are merged in as the final sections of the
        // Account page, in that order.
        let teams = ctx.add_typed_action_view(TeamsPageView::new);
        ctx.subscribe_to_view(&teams, |_, _, event, ctx| {
            ctx.emit(MainSettingsPageEvent::Teams(event.clone()));
        });
        widgets.push(Box::new(TeamsSectionWidget));

        let billing_and_usage = ctx.add_view(BillingAndUsageDispatchView::new);
        ctx.subscribe_to_view(&billing_and_usage, |_, _, event, ctx| {
            ctx.emit(MainSettingsPageEvent::Billing(event.clone()));
        });
        widgets.push(Box::new(BillingAndUsageWidget::default()));

        let page = PageType::new_uncategorized(widgets, Some("Account"));

        MainSettingsPageView {
            page,
            auth_state,
            billing_and_usage,
            teams,
            avatar_picker_modal,
            avatar_picker_body,
        }
    }

    /// The page's active modal, if any: the avatar picker, or whatever the
    /// embedded billing section has open.
    pub fn modal_content(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if self.avatar_picker_modal.is_open() {
            return Some(self.avatar_picker_modal.render());
        }
        self.billing_and_usage
            .read(app, |view, _| view.get_modal_content(app))
    }

    fn handle_autoupdate_state_change(
        &mut self,
        _: ModelHandle<AutoupdateState>,
        ctx: &mut ViewContext<Self>,
    ) {
        ctx.notify();
    }
}

#[derive(Default)]
struct AccountWidgetStateHandles {
    upgrade_link: MouseStateHandle,
    anonymous_user_sign_up_button: MouseStateHandle,
    enterprise_contact_us_link: MouseStateHandle,
    stripe_billing_portal_link: MouseStateHandle,
    avatar: MouseStateHandle,
    log_out_button: MouseStateHandle,
}

#[derive(Default)]
struct AccountWidget {
    ui_state_handles: AccountWidgetStateHandles,
}

impl AccountWidget {
    /// The one avatar on the page: your chosen picture, or your initial until
    /// you choose one. Clicking it opens the picker modal — it is the way in,
    /// so it gets a hand cursor and an accent ring on hover.
    fn render_clickable_avatar(
        &self,
        source: Option<&AssetSource>,
        auth_state: &AuthState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let accent = theme.accent();
        let surface = theme.surface_3();
        let text_color = theme.active_ui_text_color();
        let font = appearance.ui_font_family();
        let source = source.cloned();
        let initial = auth_state
            .username_for_display()
            .and_then(|name| name.chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        Hoverable::new(self.ui_state_handles.avatar.clone(), move |mouse| {
            let border = if mouse.is_hovered() {
                Border::all(2.).with_border_fill(accent.clone())
            } else {
                Border::all(2.).with_border_color(ColorU::transparent_black())
            };
            let picture: Box<dyn Element> = match &source {
                Some(source) => Image::new(source.clone(), CacheOption::BySize)
                    .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                    .finish(),
                None => Container::new(
                    Align::new(
                        Text::new_inline(initial.clone(), font, 20.)
                            .with_color(text_color.clone().into_solid())
                            .finish(),
                    )
                    .finish(),
                )
                .with_background(surface.clone())
                .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                .finish(),
            };
            Container::new(
                ConstrainedBox::new(picture)
                    .with_width(AVATAR_DISPLAY_SIZE)
                    .with_height(AVATAR_DISPLAY_SIZE)
                    .finish(),
            )
            .with_border(border)
            .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
            .with_uniform_padding(2.)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(MainPageAction::OpenAvatarPicker))
        .finish()
    }

    fn render_anonymous_account_info(
        &self,
        avatar_source: Option<&AssetSource>,
        auth_state: &AuthState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let button_styles = UiComponentStyles {
            font_size: Some(14.),
            font_weight: Some(Weight::Semibold),
            border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
            padding: Some(Coords {
                top: 12.,
                bottom: 12.,
                left: 40.,
                right: 40.,
            }),
            ..Default::default()
        };

        let user_info = appearance
            .ui_builder()
            .button(
                ButtonVariant::Accent,
                self.ui_state_handles.anonymous_user_sign_up_button.clone(),
            )
            .with_style(button_styles)
            .with_text_label("Sign up".to_owned())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(MainPageAction::SignupAnonymousUser);
            })
            .finish();

        let mut plan_info = Flex::column()
            .with_main_axis_alignment(MainAxisAlignment::SpaceEvenly)
            .with_cross_axis_alignment(CrossAxisAlignment::End);
        let current_user_id = auth_state.user_id().unwrap_or_default();

        plan_info.add_child(render_customer_type_badge(appearance, "Free".into()));
        plan_info.add_child(
            Container::new(
                appearance
                    .ui_builder()
                    .button(
                        ButtonVariant::Link,
                        self.ui_state_handles.upgrade_link.clone(),
                    )
                    .with_text_and_icon_label(
                        TextAndIcon::new(
                            TextAndIconAlignment::IconFirst,
                            "Compare plans",
                            Icon::CoinsStacked.to_borkui_icon(appearance.theme().accent()),
                            MainAxisSize::Min,
                            MainAxisAlignment::Center,
                            vec2f(14., 14.),
                        )
                        .with_inner_padding(4.),
                    )
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(MainPageAction::Upgrade {
                            team_uid: None,
                            user_id: current_user_id,
                        });
                    })
                    .finish(),
            )
            .with_margin_top(8.)
            .finish(),
        );

        Flex::row()
            .with_child(
                Shrinkable::new(
                    1.0,
                    Flex::row()
                        .with_child(
                            // Your avatar is local identity: it works signed
                            // out, so the picker is reachable here too.
                            Container::new(self.render_clickable_avatar(
                                avatar_source,
                                auth_state,
                                appearance,
                            ))
                            .with_margin_right(HEADER_PADDING)
                            .finish(),
                        )
                        .with_child(user_info)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_main_axis_alignment(MainAxisAlignment::Start)
                        .with_main_axis_size(MainAxisSize::Max)
                        .finish(),
                )
                .finish(),
            )
            .with_child(Align::new(plan_info.finish()).right().finish())
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .finish()
    }

    fn render_account_info(
        &self,
        profile_image_source: Option<&AssetSource>,
        auth_state: &AuthState,
        app: &AppContext,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut user_info = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        // The one avatar, and the door to the picker modal.
        user_info.add_child(
            Container::new(self.render_clickable_avatar(
                profile_image_source,
                auth_state,
                appearance,
            ))
            .with_margin_right(HEADER_PADDING)
            .finish(),
        );

        let display_name = auth_state.username_for_display().map(|screen_name| {
            let email = auth_state.user_email();
            match email {
                Some(email) => {
                    if !screen_name.is_empty() && screen_name != email {
                        Flex::column()
                            .with_main_axis_alignment(MainAxisAlignment::SpaceEvenly)
                            .with_cross_axis_alignment(CrossAxisAlignment::Start)
                            .with_child(
                                Text::new_inline(screen_name, appearance.ui_font_family(), 16.)
                                    .with_color(appearance.theme().active_ui_text_color().into())
                                    .finish(),
                            )
                            .with_child(
                                appearance
                                    .ui_builder()
                                    .paragraph(email)
                                    .with_style(UiComponentStyles {
                                        font_color: Some(
                                            appearance
                                                .theme()
                                                .active_ui_text_color()
                                                .with_opacity(60)
                                                .into(),
                                        ),
                                        font_size: Some(REGULAR_TEXT_FONT_SIZE),
                                        ..Default::default()
                                    })
                                    .build()
                                    .finish(),
                            )
                            .finish()
                    } else {
                        Text::new_inline(email, appearance.ui_font_family(), 16.)
                            .with_color(appearance.theme().active_ui_text_color().into())
                            .finish()
                    }
                }
                _ => Text::new_inline(screen_name, appearance.ui_font_family(), 16.)
                    .with_color(appearance.theme().active_ui_text_color().into())
                    .finish(),
            }
        });

        if let Some(display_name) = display_name {
            user_info.add_child(display_name);
        }

        let mut plan_info = Flex::column()
            .with_main_axis_alignment(MainAxisAlignment::SpaceEvenly)
            .with_cross_axis_alignment(CrossAxisAlignment::End);
        let current_user_id = auth_state.user_id().unwrap_or_default();
        let workspaces = UserWorkspaces::as_ref(app);
        if let Some(team) = workspaces.current_team() {
            if team.billing_metadata.customer_type != CustomerType::Unknown {
                plan_info.add_child(render_customer_type_badge(
                    appearance,
                    team.billing_metadata.customer_type.to_display_string(),
                ));
            }

            let current_user_email = auth_state.user_email().unwrap_or_default();
            let has_admin_permissions = team.has_admin_permissions(&current_user_email);
            if has_admin_permissions {
                if team.billing_metadata.customer_type == CustomerType::Enterprise {
                    plan_info.add_child(
                        appearance
                            .ui_builder()
                            .link(
                                "Contact support".into(),
                                Some("mailto:support@bork.baby".into()),
                                None,
                                self.ui_state_handles.enterprise_contact_us_link.clone(),
                            )
                            .soft_wrap(false)
                            .build()
                            .with_margin_top(8.)
                            .finish(),
                    );
                } else {
                    if team.has_billing_history {
                        let team_uid = team.uid;
                        plan_info.add_child(
                            appearance
                                .ui_builder()
                                .link(
                                    "Manage billing".into(),
                                    None,
                                    Some(Box::new(move |ctx| {
                                        ctx.dispatch_typed_action(
                                            MainPageAction::GenerateStripeBillingPortalLink {
                                                team_uid,
                                            },
                                        );
                                    })),
                                    self.ui_state_handles.stripe_billing_portal_link.clone(),
                                )
                                .soft_wrap(false)
                                .build()
                                .with_margin_top(8.)
                                .finish(),
                        );
                    }

                    // If the team is upgradeable to self-serve tier, show them the upgrade link.
                    if team.billing_metadata.can_upgrade_to_higher_tier_plan() {
                        let description = match team.billing_metadata.customer_type {
                            CustomerType::Prosumer => "Upgrade to Turbo plan",
                            CustomerType::Turbo => "Upgrade to Lightspeed plan",
                            _ => "Compare plans",
                        };
                        let team_uid = team.uid;
                        plan_info.add_child(
                            appearance
                                .ui_builder()
                                .link(
                                    description.into(),
                                    None,
                                    Some(Box::new(move |ctx| {
                                        ctx.dispatch_typed_action(MainPageAction::Upgrade {
                                            team_uid: Some(team_uid),
                                            user_id: current_user_id,
                                        });
                                    })),
                                    self.ui_state_handles.upgrade_link.clone(),
                                )
                                .soft_wrap(false)
                                .build()
                                .with_margin_top(8.)
                                .finish(),
                        );
                    }
                }
            }
        } else {
            let plan_badge_child = render_customer_type_badge(appearance, "Free".into());
            plan_info.add_child(plan_badge_child);

            plan_info.add_child(
                appearance
                    .ui_builder()
                    .link(
                        "Compare plans".into(),
                        None,
                        Some(Box::new(move |ctx| {
                            ctx.dispatch_typed_action(MainPageAction::Upgrade {
                                team_uid: None,
                                user_id: current_user_id,
                            });
                        })),
                        self.ui_state_handles.upgrade_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .with_margin_top(8.)
                    .finish(),
            );
        }

        let mut row = Flex::row()
            .with_child(
                Shrinkable::new(1.0, Align::new(user_info.finish()).left().finish()).finish(),
            )
            .with_cross_axis_alignment(CrossAxisAlignment::Center);

        if !FeatureFlag::UsageBasedPricing.is_enabled() {
            row.add_child(Align::new(plan_info.finish()).right().finish());
        }

        // Log out belongs on the account row itself, across from the identity
        // it signs out — not as a stray button lower on the page.
        row.add_child(
            Container::new(self.render_log_out_button(appearance))
                .with_margin_left(HEADER_PADDING)
                .finish(),
        );

        row.finish()
    }

    fn render_log_out_button(&self, appearance: &Appearance) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .button(
                ButtonVariant::Secondary,
                self.ui_state_handles.log_out_button.clone(),
            )
            .with_text_label(LOG_OUT_TEXT.into())
            .with_style(UiComponentStyles {
                font_size: Some(14.),
                padding: Some(Coords::uniform(8.).left(24.).right(24.)),
                ..Default::default()
            })
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::LogOut);
            })
            .finish()
    }
}

impl SettingsWidget for AccountWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "account sign up"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        // Fork: the avatar is local identity — a natively generated DiceBear
        // avatar or an uploaded photo — not the (dead) account photo URL. It
        // renders (and opens its picker) signed in or out.
        let profile_image_source = IdentitySettings::selected_avatar_source(app);
        let account_info = if view.auth_state.is_anonymous_or_logged_out() {
            self.render_anonymous_account_info(
                profile_image_source.as_ref(),
                view.auth_state.as_ref(),
                appearance,
            )
        } else {
            self.render_account_info(
                profile_image_source.as_ref(),
                view.auth_state.as_ref(),
                app,
                appearance,
            )
        };

        Flex::column()
            .with_child(
                Container::new(account_info)
                    .with_margin_top(VERTICAL_MARGIN)
                    .finish(),
            )
            .finish()
    }
}

/// Invitation to create an account, at the top of the page.
///
/// The fork works fully without an account; one only buys the optional cloud
/// services (settings sync, teams). There is no signup flow yet — the account
/// server exists but the client is not wired to it (ACCOUNT-DESIGN.md phase
/// 5) — so this deliberately carries no call-to-action button rather than a
/// dead one. Give it an `on_click` when signup lands.
#[derive(Default)]
struct SignupBannerWidget;

impl SettingsWidget for SignupBannerWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "sign up signup account settings sync teams cloud"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        // The pitch is for the signed-out: anyone with a sign-in (legacy auth
        // or our account) shouldn't see it at all.
        AuthStateProvider::as_ref(app)
            .get()
            .is_anonymous_or_logged_out()
            && !crate::account::has_account()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let body = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Text::new_inline(
                    "Sign up for settings sync and teams",
                    appearance.ui_font_family(),
                    14.,
                )
                .with_color(theme.active_ui_text_color().into())
                .finish(),
            )
            .with_child(
                Container::new(
                    Text::new(
                        "Everything here works without an account. An account only adds \
                         syncing your settings across machines and sharing them with a team."
                            .to_string(),
                        appearance.ui_font_family(),
                        13.,
                    )
                    .with_color(theme.sub_text_color(theme.surface_1()).into())
                    .finish(),
                )
                .with_margin_top(4.)
                .finish(),
            )
            .finish();

        // Stretch to the same column width as every other section: the card
        // fills the row rather than hugging its text.
        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(
                    Expanded::new(
                        1.,
                        Container::new(body)
                            .with_horizontal_padding(12.)
                            .with_vertical_padding(12.)
                            .with_background(theme.surface_1())
                            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                            .with_border(Border::new(1.).with_border_fill(theme.outline()))
                            .finish(),
                    )
                    .finish(),
                )
                .finish(),
        )
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

/// Teams, embedded as a section of this page.
///
/// Teams was a settings page of its own; the feature is unchanged — you can
/// still create a team and bring people into it — it just lives here now,
/// because team membership is part of an account. Deep links to the old
/// `SettingsSection::Teams` redirect to this section.
#[derive(Default)]
struct TeamsSectionWidget;

/// Stable id so callers that used to open the standalone Teams page can
/// scroll to this section instead.
pub const TEAM_WIDGET_ID: &str = "team";

impl SettingsWidget for TeamsSectionWidget {
    type View = MainSettingsPageView;

    fn widget_id(&self) -> &'static str {
        TEAM_WIDGET_ID
    }

    fn search_terms(&self) -> &str {
        "team teams members invites organization organisation"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(
                    Container::new(Empty::new().finish())
                        .with_border(
                            Border::bottom(1.).with_border_fill(appearance.theme().outline()),
                        )
                        .finish(),
                )
                .with_child(
                    Container::new(render_sub_header(appearance, "Teams", None))
                        .with_margin_top(VERTICAL_MARGIN)
                        .finish(),
                )
                .with_child(ChildView::new(&view.teams).finish())
                .finish(),
        )
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

struct DividerWidget {}

impl SettingsWidget for DividerWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        ""
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(
            Container::new(Empty::new().finish())
                .with_border(Border::bottom(1.).with_border_fill(appearance.theme().outline()))
                .finish(),
        )
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

#[derive(Default)]
struct SettingsSyncWidget {
    tooltip_state: MouseStateHandle,
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for SettingsSyncWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "settings sync"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        !AuthStateProvider::as_ref(app)
            .get()
            .is_anonymous_or_logged_out()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let preferences_settings = CloudPreferencesSettings::as_ref(app);

        let label_info = AdditionalInfo {
            mouse_state: self.tooltip_state.clone(),
            on_click_action: Some(MainPageAction::OpenUrl(
                "https://therob.lol/bork-terminal/help/terminal/more-features/settings-sync".into(),
            )),
            secondary_text: None,
            tooltip_override_text: None,
        };

        Container::new(render_body_item::<MainPageAction>(
            "Settings sync".to_string(),
            Some(label_info),
            // Cloud prefs are always synced, so no need to show the local-only icon.
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*preferences_settings.settings_sync_enabled.value())
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(MainPageAction::ToggleSettingsSync)
                })
                .finish(),
            None,
        ))
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

#[derive(Default)]
struct VersionInfoWidget {
    copy_version_button_mouse_state: MouseStateHandle,
    version_info_cta_link_mouse_state: MouseStateHandle,
}

impl VersionInfoWidget {
    fn render_version_info(
        &self,
        version: &'static str,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let faded_text_color = appearance
            .theme()
            .active_ui_text_color()
            .with_opacity(60)
            .into();
        struct StatusContent {
            text: &'static str,
            color: ColorU,
        }
        struct CallToActionContent {
            text: &'static str,
            action: MainPageAction,
        }

        let (status_content, call_to_action_content) =
            if ContextFlag::PromptForVersionUpdates.is_enabled() {
                let ansi_red: ColorU = appearance.theme().terminal_colors().bright.red.into();
                match autoupdate::get_update_state(app) {
                    AutoupdateStage::NoUpdateAvailable => (
                        Some(StatusContent {
                            text: "Up to date",
                            color: faded_text_color,
                        }),
                        Some(CallToActionContent {
                            text: "Check for updates",
                            action: MainPageAction::CheckForUpdate,
                        }),
                    ),
                    AutoupdateStage::CheckingForUpdate => (
                        Some(StatusContent {
                            text: "checking for update...",
                            color: faded_text_color,
                        }),
                        None,
                    ),
                    AutoupdateStage::DownloadingUpdate => (
                        Some(StatusContent {
                            text: "downloading update...",
                            color: faded_text_color,
                        }),
                        None,
                    ),
                    AutoupdateStage::UpdateReady { .. } => (
                        Some(StatusContent {
                            text: "Update available",
                            color: ansi_red,
                        }),
                        Some(CallToActionContent {
                            text: "Relaunch Bork",
                            action: MainPageAction::Relaunch,
                        }),
                    ),
                    AutoupdateStage::Updating { .. } => (
                        Some(StatusContent {
                            text: "Updating...",
                            color: faded_text_color,
                        }),
                        None,
                    ),
                    AutoupdateStage::UpdatedPendingRestart { .. } => (
                        Some(StatusContent {
                            text: "Installed update",
                            color: faded_text_color,
                        }),
                        Some(CallToActionContent {
                            text: "Relaunch Bork",
                            action: MainPageAction::Relaunch,
                        }),
                    ),
                    AutoupdateStage::UnableToUpdateToNewVersion { .. } => (
                        Some(StatusContent {
                            text: "A new version of Bork is available but can't be installed",
                            color: ansi_red,
                        }),
                        Some(CallToActionContent {
                            text: "Update Bork manually",
                            // note: the handler for this action is a no-op
                            action: MainPageAction::DownloadUpdate,
                        }),
                    ),
                    AutoupdateStage::UnableToLaunchNewVersion { .. } => (
                        Some(StatusContent {
                            text: "A new version of Bork is installed but can't be launched.",
                            color: ansi_red,
                        }),
                        Some(CallToActionContent {
                            text: "Update Bork manually",
                            // note: the handler for this action is a no-op
                            action: MainPageAction::DownloadUpdate,
                        }),
                    ),
                }
            } else {
                (None, None)
            };

        let mut first_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Shrinkable::new(
                    1.0,
                    Align::new(
                        Text::new_inline(
                            "Version".to_string(),
                            appearance.ui_font_family(),
                            REGULAR_TEXT_FONT_SIZE,
                        )
                        .with_color(faded_text_color)
                        .finish(),
                    )
                    .left()
                    .finish(),
                )
                .finish(),
            );
        if let Some(call_to_action_content) = call_to_action_content {
            first_row.add_child(
                appearance
                    .ui_builder()
                    .link(
                        call_to_action_content.text.into(),
                        None,
                        Some(Box::new(move |ctx| {
                            ctx.dispatch_typed_action(call_to_action_content.action.clone());
                        })),
                        self.version_info_cta_link_mouse_state.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            );
        }

        let mut second_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Shrinkable::new(
                    1.0,
                    Align::new(
                        Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Start)
                            .with_child(
                                appearance
                                    .ui_builder()
                                    .copy_button(16., self.copy_version_button_mouse_state.clone())
                                    .build()
                                    .with_cursor(Cursor::PointingHand)
                                    .on_click(move |ctx, _, _| {
                                        ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(
                                            version,
                                        ));
                                    })
                                    .finish(),
                            )
                            .with_child(
                                Container::new(
                                    Text::new_inline(
                                        version.to_string(),
                                        appearance.ui_font_family(),
                                        REGULAR_TEXT_FONT_SIZE,
                                    )
                                    .with_color(appearance.theme().active_ui_text_color().into())
                                    .finish(),
                                )
                                .with_margin_left(8.)
                                .finish(),
                            )
                            .finish(),
                    )
                    .left()
                    .finish(),
                )
                .finish(),
            );
        if let Some(status_content) = status_content {
            second_row.add_child(
                Text::new_inline(
                    status_content.text.to_string(),
                    appearance.ui_font_family(),
                    REGULAR_TEXT_FONT_SIZE,
                )
                .with_color(status_content.color)
                .finish(),
            );
        }

        let mut version_info = Flex::column();
        version_info.add_child(first_row.finish());
        version_info.add_child(
            Container::new(second_row.finish())
                .with_margin_top(5.)
                .finish(),
        );
        version_info.finish()
    }
}

impl SettingsWidget for VersionInfoWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "version update"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if let Some(version) = ChannelState::app_version() {
            Container::new(self.render_version_info(version, appearance, app))
                .with_margin_top(VERTICAL_MARGIN)
                .finish()
        } else {
            report_error!("Shouldn't render VersionInfoWidget without GIT_RELEASE_TAG");
            Empty::new().finish()
        }
    }
}

/// Widget displaying IAP credential state and a refresh button. Only
/// visible on staging channels where IAP is active.
#[cfg(not(target_family = "wasm"))]
#[derive(Default)]
struct IapCredentialsWidget {
    refresh_button_mouse_state: MouseStateHandle,
}

#[cfg(not(target_family = "wasm"))]
impl SettingsWidget for IapCredentialsWidget {
    type View = MainSettingsPageView;

    fn search_terms(&self) -> &str {
        "iap staging gcloud proxy credentials"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        // `is_enabled()` gates widget registration in `MainSettingsPageView::new`,
        // so `state()` should be `Some` here; bail out defensively though.
        let Some(state) = IapManager::as_ref(app).state() else {
            return Empty::new().finish();
        };
        let ansi_red: ColorU = appearance.theme().terminal_colors().bright.red.into();
        let disabled: ColorU = appearance.theme().disabled_ui_text_color().into();
        let active: ColorU = appearance.theme().active_ui_text_color().into();
        let (status_text, status_color): (String, ColorU) = match &state {
            IapCredentialsState::Missing => ("Not yet loaded".to_string(), disabled),
            IapCredentialsState::Refreshing { .. } => ("Refreshing…".to_string(), active),
            IapCredentialsState::Loaded(cached) => {
                let remaining = cached
                    .expires_at
                    .saturating_duration_since(instant::Instant::now());
                let mins = remaining.as_secs() / 60;
                (format!("Loaded (refreshes in ~{mins}m)"), active)
            }
            IapCredentialsState::Failed { message, .. } => (format!("Failed: {message}"), ansi_red),
            IapCredentialsState::EnvInjected { .. } => {
                ("Using injected token (BORK_IAP_TOKEN)".to_string(), active)
            }
        };

        let is_refreshing = matches!(state, IapCredentialsState::Refreshing { .. });

        let label = Align::new(
            Text::new_inline(
                "Staging IAP credentials".to_string(),
                appearance.ui_font_family(),
                REGULAR_TEXT_FONT_SIZE,
            )
            .with_color(appearance.theme().active_ui_text_color().into())
            .finish(),
        )
        .left()
        .finish();

        let status = Container::new(
            appearance
                .ui_builder()
                .paragraph(status_text)
                .with_style(UiComponentStyles {
                    font_color: Some(status_color),
                    font_size: Some(REGULAR_TEXT_FONT_SIZE),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .with_margin_top(4.)
        .finish();

        let refresh_button = appearance
            .ui_builder()
            .button(
                ButtonVariant::Secondary,
                self.refresh_button_mouse_state.clone(),
            )
            .with_text_label(if is_refreshing {
                "Refreshing…".into()
            } else {
                "Refresh".into()
            })
            .with_style(UiComponentStyles {
                font_size: Some(12.),
                padding: Some(Coords::uniform(6.).left(16.).right(16.)),
                ..Default::default()
            })
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(MainPageAction::RefreshIapCredentials);
            })
            .finish();

        let button_row = Container::new(Align::new(refresh_button).left().finish())
            .with_margin_top(8.)
            .finish();

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(label)
                .with_child(status)
                .with_child(button_row)
                .finish(),
        )
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

/// Renders the billing & usage section at the bottom of the Account page.
/// The heavy lifting lives in [`BillingAndUsageDispatchView`], which picks
/// between the v1 and v2 billing pages.
#[derive(Default)]
struct BillingAndUsageWidget;

/// Stable id so callers that used to navigate to the standalone
/// "Billing and usage" page can scroll straight to this section instead.
pub const BILLING_AND_USAGE_WIDGET_ID: &str = "billing_and_usage";

impl SettingsWidget for BillingAndUsageWidget {
    type View = MainSettingsPageView;

    fn widget_id(&self) -> &'static str {
        BILLING_AND_USAGE_WIDGET_ID
    }

    fn search_terms(&self) -> &str {
        "plan billing a.i. ai usage limit credits balance overview"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        self.dispatch_should_render(app)
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(
                    Container::new(Empty::new().finish())
                        .with_border(
                            Border::bottom(1.).with_border_fill(appearance.theme().outline()),
                        )
                        .finish(),
                )
                .with_child(
                    Container::new(render_sub_header(appearance, "Billing and usage", None))
                        .with_margin_top(VERTICAL_MARGIN)
                        .finish(),
                )
                .with_child(ChildView::new(&view.billing_and_usage).finish())
                .finish(),
        )
        .with_margin_top(VERTICAL_MARGIN)
        .finish()
    }
}

impl BillingAndUsageWidget {
    /// `SettingsWidget::should_render` only gets an `AppContext`, but the
    /// anonymous-user check lives on the dispatch view, so read it through the
    /// singleton auth state the same way the dispatch view does.
    fn dispatch_should_render(&self, app: &AppContext) -> bool {
        !AuthStateProvider::as_ref(app)
            .get()
            .is_anonymous_or_logged_out()
    }
}

impl SettingsPageMeta for MainSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Account
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn on_page_selected(&mut self, allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        // We want to immediately see if the user is part of a workspace rather than wait for the next poll.
        std::mem::drop(
            TeamUpdateManager::handle(ctx)
                .update(ctx, |manager, ctx| manager.refresh_workspace_metadata(ctx)),
        );
        self.billing_and_usage.update(ctx, |view, ctx| {
            view.on_page_selected(allow_steal_focus, ctx);
        });
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

impl From<ViewHandle<MainSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<MainSettingsPageView>) -> Self {
        SettingsPageViewHandle::Main(view_handle)
    }
}
