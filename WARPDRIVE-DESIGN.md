# Warp Drive, local-first: design and implementation

This document explains how this fork converted Warp Drive — Warp's cloud
notes/workflows/notebooks workspace — into a **fully local, always-on,
serverless** feature, how the approach was chosen, and the technical
specifics of every layer touched. The companion maintainer map (shorter,
trap-focused, kept current) is [docs/local-drive.md](docs/local-drive.md).

Landed in commit `071306a` ("Make Warp Drive fully local"), 2026-07-19.

---

## 1. Goal

Upstream Warp Drive stores notebooks, workflows, environment-variable
collections, MCP server configs, AI facts, and folders on Warp's servers,
gates the feature behind an account, limits anonymous users, and shares via
`warp.dev` links. This fork wanted:

1. Everything stored locally, owned by the user.
2. No central server involvement, ever.
3. Sharing = export a file, send it, the other person imports it.
4. Drive as a permanent, ungated part of the app — no setting, no login,
   no limits. An empty Drive is just an empty panel.
5. Other cloud plumbing (sessions, teams infra outside Drive, future
   custom cloud features) explicitly left alone.

## 2. The decisive discovery: Drive was already offline-first in shape

Before writing anything, the investigation mapped how data flows. That map
is what made the whole project cheap:

```
UI (drive/panel, drive/index, item views, pane views)
   ⇅
CloudModel (app/src/cloud_object/model/persistence.rs)
   in-memory source of truth, loaded from SQLite at startup
   ⇅ PersistenceWriter (single writer thread) → SQLite
   ⇅
SyncQueue        — pushes creates/updates ("content" changes)
UpdateManager    — periodic polling + online-only ops (trash, move, …)
Listener         — websocket for remote updates (already disabled in fork)
   ⇘ all three talk through ONE trait object:
      Arc<dyn ObjectClient>   (≈30 async methods)
```

Three properties of upstream's own design carried the project:

* **Every mutation is optimistic.** The UI writes CloudModel and SQLite
  through the normal persistence path *first*, then calls the client and
  only needs a successful acknowledgement. The server was almost never the
  source of data the client didn't already have.
* **Creation is local-first.** New objects immediately get a `ClientId`
  (UUID); the SyncQueue later "creates" them server-side and patches in the
  returned 22-character `ServerId`.
* **The server's entire surface is one trait**, `ObjectClient`
  (`crates/cloud_object_client`), already mockable, with the GraphQL
  implementation living in `app/src/server/server_api/object.rs` and one
  provider chokepoint: `ServerApiProvider::get_cloud_objects_client()`.

So instead of rewriting a 22k-line UI module, the fork **replaces what is
behind the trait**. Everything upstream keeps working because, from the
app's perspective, there is still a "server" — it's just local, instant,
and always says yes.

A second discovery shaped the details: several flows **early-return for
objects that have no ServerId** (`UpdateManager::trash_object`,
`untrash_object`, `delete_object_with_initiated_by`, and foldering —
`folder_id` columns/requests are ServerId-based). A logged-out upstream
client silently can't trash anything. That ruled out the "never sync,
live on ClientIds" design and mandated fabricated server IDs.

## 3. `LocalObjectClient`

`app/src/server/cloud_objects/local_object_client.rs` implements
`ObjectClient` as a stateless local stand-in:

* **Creates** (`create_workflow` / `create_notebook` / `create_folder` /
  `create_generic_string_object` / bulk): fabricate a `CreatedCloudObject`
  from the request — client_id echoed, `ServerId` = 22 hex chars of a v4
  UUID (88 bits; `ServerId::from_string_lossy` debug-panics on any other
  length, hence the exact slice), revision = now, private owner-only
  permissions, `creator_uid: None`.
* **Updates**: `UpdateCloudObjectResult::Success` with a fresh revision.
  Revisions are *timestamps* (`Revision(ServerTimestamp)`), and with a
  single writer "now" is always the latest, so conflicts can't occur and
  the `Rejected` conflict path is dead.
* **Trash / move**: `Ok(true)` — the UpdateManager already applied the
  change locally and persists it on acknowledgement.
* **Three calls where the real server contributed knowledge** answer from
  a **read-only SQLite connection** (`establish_ro_connection`, an
  existing blessed helper) to the same database the app persists to:
  * `untrash_object` — the one flow that is *not* optimistic upstream: the
    consumer waits for the server's metadata echo and applies it via
    `update_from_new_metadata_ts` (which uses `current_editor_uid`,
    `trashed_ts`, `folder_id`, `creator_uid`, and the metadata timestamp).
    The client reads the object's row and echoes its metadata with
    `trashed_ts: None`.
  * `delete_object` — the server used to compute folder deletion cascades.
    Locally, a BFS over `object_metadata` follows the folder tree and
    returns every descendant `SyncId`.
  * `empty_trash` — returns everything with `trashed_ts IS NOT NULL`. The
    `owner` filter is deliberately ignored: one user, one space.
* **`fetch_changed_objects`** returns an empty `InitialLoadResponse` —
  SQLite is the source of truth and CloudModel loaded it at startup;
  nothing is ever new "server-side". (The MCP gallery and the cloud
  preferences syncer ride this same response/client; the gallery is now
  empty and preference "sync" is a harmless local no-op.)
* **`get_warp_drive_updates`** signals stream-readiness, then pends
  forever — returning would make the Listener treat it as a disconnection
  and reconnect-loop.
* **Sharing, guests, link permissions, ownership transfer,
  `fetch_single_cloud_object`, `leave_object`**: `Err`. They have no local
  meaning and their UI entry points were removed.

Two SQLite encoding details bit during implementation and are now locked
in by tests:

* The `object_metadata.server_id` column does **not** store the bare uid —
  it stores `"{TypePrefix}-{uid}"` (`sqlite_type_and_uid_hash`), e.g.
  `Workflow-<22 chars>`. Lookups from a bare `ServerId` therefore match
  all four prefixed forms, and IDs read back are stripped with
  `parse_sqlite_id_to_uid` before parsing.
* The `folder_id` column stores the parent folder's sqlite hash —
  `"Folder-{uid}"` once acknowledged, `"Client-{uuid}"` before — so the
  cascade walk's frontier carries those hash forms verbatim.

**Writes never happen from the client.** The app has exactly one SQLite
writer (the persistence writer thread); the local client's answers are
applied and persisted by the normal UpdateManager/SyncQueue consumers.
This is the single most important invariant for anyone touching this code.

Objects created back when the install talked to warp.dev keep their real
server IDs and behave identically — the client acknowledges operations on
any ID without caring where it came from.

### Wiring

`ServerApiProvider::get_cloud_objects_client()` (app/src/server/server_api.rs)
returns `Arc::new(LocalObjectClient::new())` instead of the GraphQL-backed
`ServerApi`. That one seam covers SyncQueue, UpdateManager, Listener, and
the preferences syncer — no GraphQL client is ever constructed for Drive.

### Startup sequence

`SyncQueue` boots with dequeueing disabled; upstream enabled it only after
the first successful poll, and polling only started on login. The fork
kicks polling off in `lib.rs` immediately after the `UpdateManager`
singleton registers:

```
lib.rs → start_polling_for_updated_objects()      (no login / is_online gate)
  → LocalObjectClient::fetch_changed_objects      (instant, empty)
  → on_changed_objects_fetched: has_initial_load.set()
  → SyncQueue::start_dequeueing()                 ← mutations now flow
```

Deliberately **not** inside `UpdateManager::new()`: tests construct the
manager with scripted `MockObjectClient`s and drive polling explicitly;
an auto-poll in the constructor breaks their call-count expectations. The
production kick lives at the production wiring point only.

Network transitions are ignored (`Offline` no longer stops polling or
dequeueing) — the "server" works in airplane mode, and the old
reconnect-refresh semantics are meaningless locally.

## 4. What was removed, and the single-chokepoint trick

Wherever possible the removal was one load-bearing accessor rather than
N call sites:

* **Anonymous object limits + "Sign up for free" storage banner**:
  `AuthState::personal_object_limits()` (crates/warp_server_auth) now
  returns `None`. Every limit gate ANDs on it and the banner `?`-unwraps
  it, so one change disabled all of them.
* **Pane-header Share button** for Drive-object panes:
  `is_sharing_dialog_enabled` (pane_group/pane/view/header/sharing.rs) now
  requires a shared-*session* target, so notebook/workflow/env-var panes
  never offer the dialog while session sharing keeps its flag-gated
  behavior. The SharingDialog component itself survives for
  session/conversation use.
* **Menus**: "Copy link", "Share", and "Open on Desktop" removed from the
  Drive folder and item context menus and from the notebook / workflow /
  env-var pane menus. `object_link()` itself was deliberately **kept** —
  notebook embedding resolves embedded items by round-tripping those URLs
  internally; only user-facing link surfaces were removed.
* **Team upsells**: the "Create a team" / "Join team" Drive panel sections
  are gone (sections builder in drive/index.rs). The shared-objects
  billing modal's only trigger was moving objects into over-quota team
  spaces — unreachable without teams — so it was left untouched.
* **Drive sharing onboarding block**:
  `check_and_trigger_drive_sharing_onboarding_block` is a kept-but-empty
  no-op so the call site stays aligned with upstream.

## 5. Drive is permanent: removing `enable_warp_drive`

The `enable_warp_drive` setting and every gate on it are gone:

* Setting removed from `WarpDriveSettings` (drive/settings.rs) along with
  `is_warp_drive_enabled()`. Stale `warp_drive.enabled` keys in existing
  user TOML/SQLite are simply never read again.
* The **Warp Drive settings page** (settings_view/warp_drive_page.rs)
  turned out to be nothing but the anonymous sign-up banner + the enable
  toggle, so the whole page was deleted: module, `SettingsSection::WarpDrive`
  variant, nav item, page registration, event plumbing, `update_page!`
  macro arm, and the `surface.settings.open` special case in
  local_control. ("WarpDrive" is no longer a resolvable settings page name.)
* The **appearance-page tools-panel toggle** (`ToolsPanelWarpDriveWidget`)
  and its action are gone.
* The **`ENABLE_WARP_DRIVE` keybinding context flag** and all its
  predicates are gone — Drive keybindings (new notebook/workflow/folder)
  now just require `Workspace`. Personal-folder creation also dropped its
  `IsOnline` predicate: local folders don't need a network.
* Gates unwrapped at: tools-panel view list, `OpenWarpDrive` /
  `ToggleWarpDrive` workspace actions, command palette filters + zero
  state, command search sources + zero state, AI context menu categories
  (@-mention Workflows/Notebooks/Plans/Rules), "Save as workflow" in
  terminal block/input menus, and `surface` destination checks.
* Onboarding: `apply_onboarding_settings` no longer writes the setting,
  and finishing onboarding never demands login for Drive. The onboarding
  crate's own UI model was left untouched (separate workstream).

Behavioral consequence worth knowing: Drive is now in the tools panel for
*every* test-app environment too, which shifted the default active
left-panel view in a couple of tests that implicitly relied on Drive being
absent for logged-out users (they now activate the Tabs panel explicitly).

## 6. Folder export (sharing whole trees)

Export previously handled single objects (notebooks → `.md`, workflows →
`.yaml`, env vars → `.env`) and bulk selections; folders bailed with "not
yet supported". Now:

* The Drive folder context menu has **Export**.
* `ExportManager::export` expands a folder into its exportable
  descendants before the exportability filter, walking
  `CloudModel::active_cloud_objects_in_location_without_descendents` and
  assigning each object a directory **relative to the picked location**
  that mirrors the Drive tree (`Folder A/Sub Folder/…`, names made
  filesystem-safe, empty names → "Untitled").
* `Export` records the relative `subdir`; `export_one` joins it under the
  picked directory (plain bulk exports keep upstream's space-named
  subdirectory behavior); `write_object` creates directories for any
  subdir'd or bulk export.
* The completion toast's "Open in Finder" opens the exported folder's
  *root* (computed by stripping the subdir depth from the last written
  file), not whatever nested subdirectory finished last.
* An export that expands to nothing (empty folder) shows a "Nothing to
  export" toast instead of opening a useless file picker.
* Import needed **zero changes**: the import modal already walks picked
  directories recursively and recreates folders, so folder export →
  send → import round-trips. (Env-var `.env` files export but are not an
  import format upstream — unchanged.)

## 7. The "why not fully decouple from the cloud code?" question

Legitimate question: if Drive is permanent and local, why does the code
still say `CloudModel`, `SyncQueue`, `UpdateManager`, `ObjectClient`?

The answer is that the fork **decoupled the behavior, not the vocabulary**:

* Functionally there is nothing left to decouple *from* — no network
  calls, no auth dependency, no server types crossing the UI. The trait
  boundary is now effectively "Drive's storage engine API", and the local
  client is its only production implementation.
* The machinery names are load-bearing across ~200 files and several
  crates, and other features (AI documents, MCP manager, preferences
  syncer, conversation models) subscribe to the same events. A
  rename/inline refactor is days of churn with real regression risk and
  zero user-visible gain.
* Keeping upstream's shapes keeps upstream-derived fixes cheap to port,
  and keeps the door open for the fork's *own* future cloud features —
  a future sync backend is "implement `ObjectClient` again" (with the one
  caveat that timestamp revisions were only conflict-safe because there's
  a single writer; a real second writer must revisit that).

If a deeper cleanup is ever wanted, the sane staging is: (1) collapse the
queue's retry/error states that can no longer occur, (2) merge
UpdateManager's "online-only op" pathways with the optimistic ones,
(3) rename the crates/modules last, purely cosmetically. None of it is
prerequisite for anything.

## 8. Testing

* **`local_object_client_tests.rs`** — the DB-read helpers run against an
  in-memory SQLite with the real embedded migrations: cascade walk across
  nesting + unsynced (client-ID-only) children, trash listing, bare-uid
  lookup across all four type prefixes, metadata echo (folder-hash → 
  `FolderId` mapping, revision passthrough, client-only parents → `None`).
  These tests exist because both encoding details in §3 were caught as
  live bugs during review-by-reading; the DB layer is where this feature
  can rot silently.
* **`test_export_folder_mirrors_structure`** — end-to-end folder export
  through the real ExportManager into a temp dir, asserting the nested
  file paths and contents.
* Existing suites were updated where they asserted removed behavior:
  drive menu items without "Share", panel sections without the team
  upsell, the offline→online model test now asserts the *absence* of a
  refetch (the mock's single `times(1)` expectation is the assertion),
  and the tools-panel test asserts Drive is always available.
* Verification bar at commit time: `cargo check` clean (0 warnings),
  clippy clean on the new code, **full `warp` lib suite green** (~5.8k
  tests).

## 9. Files (orientation, not exhaustive)

| Area | Where |
| --- | --- |
| Local client + tests | app/src/server/cloud_objects/local_object_client{,_tests}.rs |
| Provider seam | app/src/server/server_api.rs (`get_cloud_objects_client`) |
| Startup kick, no network gates | app/src/lib.rs, app/src/server/cloud_objects/update_manager.rs |
| Limits/banner kill switch | crates/warp_server_auth/src/auth_state.rs (`personal_object_limits`) |
| Menus / upsells / sharing UI | app/src/drive/index.rs, notebooks/notebook.rs, workflows/workflow_view.rs, env_vars/view/menus.rs, pane_group/…/header/sharing.rs |
| Folder export | app/src/drive/export.rs (+ export_tests.rs) |
| Setting removal | app/src/drive/settings.rs, settings_view/{mod,settings_page,appearance_page}.rs (warp_drive_page.rs deleted), workspace/{mod,view}.rs, search/*, local_control/*, settings/onboarding.rs, root_view.rs |
| Maintainer map | docs/local-drive.md |

## 10. Known leftovers / future ideas

* Auto-mirror of all Drive items as plain files in an app folder
  (Obsidian-style browsing without an export step) — approved phase-2
  idea; SQLite stays the source of truth.
* `warp.dev` shared-object web intents still parse; opening one now fails
  with a toast (`fetch_single_cloud_object` errors). Could be stripped
  with the rest of the URI plumbing someday.
* Dead-but-harmless upstream code intentionally left to keep the diff
  small: sharing dialog QR codes, `Space::Shared` rendering,
  `DriveIndexSection::CreateATeam` enum variants, the neutered onboarding
  block, `SYNC_STATUS` "Saved locally" iconography (which is now simply
  accurate).
* The MCP gallery panel is empty without Warp's catalog server; if wanted,
  it could be fed from a bundled or user-supplied template list through
  `InitialLoadResponse.mcp_gallery`.
