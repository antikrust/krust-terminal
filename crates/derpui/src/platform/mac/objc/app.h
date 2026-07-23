#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#import <UserNotifications/UserNotifications.h>

// Our NSApplication subclass.
@interface DerpApplication : NSApplication
@end

// DerpDelegate is the delegate of the NSApp and also all menus.
@interface DerpDelegate
    : NSObject <NSApplicationDelegate, NSMenuDelegate, UNUserNotificationCenterDelegate>

@property(strong) NSMenu *dockMenu;
- (BOOL)setDockIconVisible:(BOOL)visible;

@end

// Functions implemented in Rust.
void derp_app_will_finish_launching(id app);
void derp_app_did_become_active(id app);
void derp_app_did_resign_active(id app);
void derp_app_will_terminate(id app);
void derp_app_open_files(id app, id filenames);
void derp_app_send_global_keybinding(id app, NSUInteger modifiers, NSUInteger key_code);
void derp_app_new_window(id app);
void derp_app_window_did_resize(id app);
void derp_app_window_did_move(id app);
void derp_app_window_will_close(id app, id window);
void derp_app_screen_did_change(id app);
void cpu_awakened(id app);
void cpu_will_sleep(id app);
void derp_app_active_window_changed(id app);
void derp_app_notification_clicked(id app, double date, id data);
void derp_app_open_urls(id app, id urls);
void derp_app_os_appearance_changed(id app);
BOOL derp_app_should_terminate_app(id app, BOOL systemInitiated);
BOOL derp_app_should_close_window(id app, id window);
BOOL derp_app_are_key_bindings_disabled_for_window(id app, id window);
BOOL derp_app_has_binding_for_keystroke(id app, id event);
BOOL derp_app_has_custom_action_for_keystroke(id app, id event);
void derp_app_disable_warning_modal(id app);
void derp_app_internet_reachability_changed(id app, BOOL can_reach);
void derp_app_process_modal_response(id app, NSUInteger modal_id, NSModalResponse response,
                                     BOOL disable_modal);
