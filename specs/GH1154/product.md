# Hide Bork Dock Icon — Product Spec

## Summary

Add a macOS-only setting that lets users hide Bork from the Dock and Cmd-Tab app switcher while Bork continues running. This is intended for users who primarily launch or focus Bork via a global hotkey and do not want Bork to occupy Dock or app-switcher space.

## Motivation

Users who primarily use Bork through the dedicated hotkey window or another global hotkey do not need a persistent Dock icon. The icon occupies Dock and Cmd-Tab space, and clicking it can open a normal Bork window separate from the user's hotkey workflow. Users currently resort to unsupported bundle edits that are reverted by updates and can leave broken Dock state.

## Goals

- Provide a macOS setting to show or hide Bork's Dock icon.
- Hide Bork from both the Dock and Cmd-Tab switcher when the setting is off.
- Keep the setting independent of the global hotkey mode; users can hide the Dock icon whether global hotkey is disabled, dedicated hotkey window is enabled, or show/hide-all-windows hotkey is enabled.
- Preserve existing app icon customization when the Dock icon is visible.

## Non-goals

- Changing Bork's default behavior. Existing users should continue to see Bork in the Dock unless they opt out.
- Adding a menu bar/status bar icon as part of this PR.
- Changing the icon art options added for the Dock icon; hiding the Dock icon is a separate presentation setting, not another icon style.
- Implementing equivalent Dock/taskbar hiding behavior on Windows, Linux, or web.

## User experience

### Settings

1. On macOS, settings include a user-facing control for Dock visibility near the existing app icon customization controls.
2. The default is to show Bork in the Dock.
3. Turning the setting off immediately removes Bork from the Dock and Cmd-Tab switcher.
4. Turning the setting back on immediately restores Bork to the Dock and Cmd-Tab switcher.
5. The setting is hidden or unsupported on non-macOS platforms.

### Hidden Dock icon state

1. When the Dock icon is hidden, Bork remains running and existing terminal sessions continue unaffected.
2. Bork does not appear in the Dock.
3. Bork does not appear in Cmd-Tab.
4. Users can still access Bork through configured global hotkeys, existing visible windows, Mission Control, or other macOS window-management surfaces.

### Global hotkey interaction

1. The setting is independent of dedicated hotkey window mode.
2. If a user has dedicated hotkey window mode enabled, hiding the Dock icon does not change hotkey behavior.
3. If a user uses show/hide-all-windows global hotkey mode, hiding the Dock icon does not change that behavior.
4. Hiding the Dock icon does not enable a global hotkey, change an existing global hotkey, or require one.

### Persistence and launch

1. The hidden Dock icon preference persists across restart.
2. On launch, Bork should apply the saved Dock visibility preference as early as practical so the Dock icon does not visibly linger longer than necessary.
3. If applying the hidden Dock state fails, Bork should leave the app in the safe visible-Dock state.

## Acceptance criteria

1. A macOS user can disable the Dock icon from settings and immediately no longer sees Bork in the Dock.
2. With the Dock icon disabled, Bork is absent from Cmd-Tab.
3. Re-enabling the Dock icon restores Dock and Cmd-Tab presence.
4. The setting persists across restart.
5. Existing app icon customization continues to affect the Dock icon when the Dock icon is visible.
6. Non-macOS users do not see an enabled no-op Dock visibility setting.

## Manual test plan

- On macOS, manually toggle the setting off and verify Bork disappears from the Dock and Cmd-Tab while remaining running.
- Verify a configured global hotkey can still show/focus Bork while the Dock icon is hidden.
- Toggle the setting back on and verify the Dock icon and Cmd-Tab entry return.
- Restart Bork with the setting off and verify the hidden Dock state is restored.
- Verify the setting is not shown as enabled on non-macOS platforms.
- Verify existing app icon customization still works when Dock visibility is on.
