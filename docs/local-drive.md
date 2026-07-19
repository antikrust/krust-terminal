# Local Warp Drive ("Library"): notes, workflows, and notebooks without a server

> **Naming**: the user-facing name of this feature is **Library** (renamed
> 2026-07-19; the panel, menus, keybindings, and tips all say Library).
> Code identifiers, settings TOML paths, and this doc's internal
> vocabulary keep the historical `warp_drive` naming — see
> WARPDRIVE-DESIGN.md §7 for why identifiers were not churned.

This fork makes **Warp Drive fully local**. Notebooks, workflows, env var
collections, MCP server configs, AI facts, and folders live in the app's
SQLite database and never touch Warp's servers. Sharing is file-based:
export an item (`.md` for notebooks, `.yaml` for workflows), send the file,
the recipient imports it. This document maps how that works and where the
seams are. **Read the traps before changing anything.**

---

## Architecture overview

Upstream Warp Drive was already offline-first in shape:

```
UI (drive/panel, drive/index, items)
  → CloudModel (app/src/cloud_object/model/persistence.rs)   in-memory source of truth
      ⇅ PersistenceWriter → SQLite (object_metadata + per-type tables)
  → SyncQueue (server/sync_queue.rs)          pushes creates/updates to the "server"
  → UpdateManager (server/cloud_objects/update_manager.rs)   polls + online-only ops
  → Listener (server/cloud_objects/listener.rs)              websocket updates (disabled)
        ↓ all three talk through
     Arc<dyn ObjectClient>
```

Every mutation is optimistic: the app writes CloudModel + SQLite first, then
asks the `ObjectClient` and only needs a "yes". The fork exploits exactly
that seam:

* **`LocalObjectClient`** (server/cloud_objects/local_object_client.rs)
  implements `ObjectClient` as a server that always says yes, immediately.
  `ServerApiProvider::get_cloud_objects_client()` returns it, so SyncQueue,
  UpdateManager, Listener, and the cloud-preferences syncer all get it —
  no GraphQL client is ever constructed for Drive.
* Creates fabricate a 22-char server ID (UUID hex). Objects therefore leave
  the `ClientId` state immediately, which matters because trash / delete /
  move / foldering all early-return on objects with no server ID.
* The three calls where the real server contributed knowledge answer from a
  **read-only** SQLite connection to the same database (reads only; all
  writes still flow through the app's single persistence writer):
  * `untrash_object` — echoes the object's metadata with `trashed_ts: None`
    (untrash is the one non-optimistic flow; the consumer applies the echo).
  * `delete_object` — walks the folder tree for the deletion cascade.
  * `empty_trash` — lists everything trashed (owner filter intentionally
    ignored: one user, one space).
* Sharing/guests/link/transfer methods return `Err` — their UI entry points
  are removed in this fork.
* `fetch_changed_objects` returns an empty `InitialLoadResponse`;
  `get_warp_drive_updates` signals ready then pends forever.

Objects created back when the install talked to warp.dev keep their real
server IDs and work unchanged — the local client acknowledges operations on
any ID.

## Startup sequence (the part that bites)

`SyncQueue` starts with dequeueing **disabled**. Upstream enabled it only
after the first successful `fetch_changed_objects`, which only ran on
login. The fork kicks polling off unconditionally in `lib.rs` right after
the `UpdateManager` singleton is registered:

```
lib.rs: UpdateManager registered
  → start_polling_for_updated_objects()        (no is_online / login gate)
  → poll → LocalObjectClient::fetch_changed_objects → empty response
  → on_changed_objects_fetched: has_initial_load.set()
  → SyncQueue::start_dequeueing()              ← creates/updates now flow
```

Network transitions no longer stop polling or dequeueing
(`handle_network_status_changed` Offline arm is a no-op).

## What was removed from the UI

* **Anonymous object limits + "Sign up for free" banner**: neutered at the
  single choke point `AuthState::personal_object_limits()` (warp_server_auth
  crate) which now returns `None`. All limit gates AND the banner render
  path check it.
* **Drive availability**: `WarpDriveSettings::is_warp_drive_enabled` is now
  purely the user setting; upstream also required a logged-in user.
* **"Copy link" / "Share" / "Open on Desktop"** in Drive context menus
  (folder + item menus in drive/index.rs). "Export" remains and is the
  sharing story. Import lives in the same menus (`Import…`).
* **Create-a-team / Join-team upsell sections** in the Drive panel
  (sections builder in drive/index.rs).
* **Pane-header Share button** for Drive-object panes:
  `is_sharing_dialog_enabled` (pane_group/pane/view/header/sharing.rs) is
  now sessions-only. The SharingDialog component itself still exists for
  session/conversation sharing.
* **Drive sharing onboarding block**:
  `check_and_trigger_drive_sharing_onboarding_block` (workspace/view.rs) is
  a no-op.

## Library rows that open panes

Rules opens the AI-fact pane (upstream behavior). Fork: **MCP Servers**
opens `MCPServersPane` (pane_group/pane/mcp_servers_pane.rs) — the same
management view as the settings page, hosted in a split pane so the user
keeps their terminal context — instead of navigating to settings. One per
window (`MCPServersPaneManager`), not persisted across restarts
(`LeafContents::MCPServers`), and the Library row is marked selected on
open, mirroring Rules.

## Traps

* **Do not** give `LocalObjectClient` a writable SQLite connection. The app
  has exactly one writer (the persistence writer thread); the client's
  answers are applied and persisted by UpdateManager/SyncQueue consumers
  through that path. A second writer invites `database is locked`.
* **`UpdateManager::new` must not auto-poll.** Tests build it with a
  scripted `MockObjectClient` and drive polling explicitly via
  `TeamTesterStatus::initiate_data_pollers`; the production kick lives in
  lib.rs only.
* **Server IDs are exactly 22 chars** and `ServerId::from_string_lossy`
  panics in debug builds on any other length. The fabricator slices UUID
  hex to 22.
* **Folder cascade keying**: `object_metadata.folder_id` stores the parent
  folder's *server* UID, so the descendant walk only follows server IDs.
  Since the local client assigns one at creation, folders are only ever
  briefly (or never) client-ID-only.
* **Revisions are timestamps.** The local client answers every update with
  `now()`. With a single writer that's always the latest revision; if a
  second writer ever appears (sync between machines), this is the first
  thing to revisit.
* The **MCP gallery** rides the same client (`InitialLoadResponse.mcp_gallery`)
  and is now always empty. The **cloud-preferences syncer** also uses this
  client; "syncing" settings succeeds as a no-op against the local store.
* `fetch_single_cloud_object` errors, so opening a `warp.dev` shared-object
  link shows a failure toast. The web-intent plumbing still exists.

## Follow-ups considered but not done

* Folder export (export a folder as a directory of files) — ExportManager
  handles multi-object exports already; folders just aren't expandable from
  the context menu yet.
* An auto-mirror of all Drive items as plain files in an app folder
  (Obsidian-style) — phase 2 if wanted; SQLite stays the source of truth.
* Ripping out the now-dead code (sharing dialog QR codes, anonymous signup
  action, `Space::Shared` rendering, `DriveIndexSection::CreateATeam`
  variants). Left in place to keep the fork's diff small against upstream.
