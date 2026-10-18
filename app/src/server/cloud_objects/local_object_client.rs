//! Fork: local-only implementation of [`ObjectClient`] so Krust Drive never
//! talks to Krust's servers.
//!
//! Architecture note: every Drive mutation in this codebase is optimistic —
//! the `UpdateManager` / `SyncQueue` write the change into `CloudModel` and
//! SQLite through the normal persistence path, then call the object client
//! and only need a successful "server" acknowledgement (plus, for a few
//! calls, data the server is authoritative for). This client plays the role
//! of a server that always says yes, immediately:
//!
//! * Creates fabricate a 22-char server ID locally, so objects leave the
//!   `ClientId` state right away. That matters because several flows
//!   (trash, delete, move, foldering) early-return for objects with no
//!   server ID.
//! * Updates acknowledge with a fresh revision timestamp. Revisions are
//!   timestamps, and with a single writer there are never conflicts.
//! * The few calls where the real server is authoritative for *other*
//!   objects (folder deletion cascades, emptying trash) or for the object's
//!   own metadata echo (untrash) answer from a read-only connection to the
//!   same SQLite database the app persists to. Reads only — all writes
//!   still flow through the app's single persistence writer.
//! * Sharing, guests, link permissions, and ownership transfer have no
//!   local meaning; those return errors. Their UI entry points are removed
//!   in this fork (sharing is file export/import instead).
//!
//! Objects that already carry real krust.baby server IDs (from before the
//! fork) keep working unchanged: this client acknowledges operations on any
//! ID without caring where it came from.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use async_channel::Sender;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cloud_object_client::{GetCloudObjectResponse, ObjectPermissionUpdateResult};
use uuid::Uuid;
use krust_graphql::object_permissions::AccessLevel;
use krust_graphql::scalars::time::ServerTimestamp;

use crate::cloud_object::model::actions::{
    ObjectAction, ObjectActionHistory, ObjectActionSubtype, ObjectActionType,
};
use crate::cloud_object::model::generic_string_model::GenericStringObjectId;
use crate::cloud_object::{
    BulkCreateCloudObjectResult, BulkCreateGenericStringObjectsRequest, CreateCloudObjectResult,
    CreateObjectRequest, CreatedCloudObject, GenericStringObjectFormat,
    GenericStringObjectUniqueKey, ObjectDeleteResult, ObjectIdType, ObjectMetadataUpdateResult,
    ObjectPermissionsUpdateData, ObjectType, ObjectsToUpdate, Owner, Revision,
    RevisionAndLastEditor, ServerFolder, ServerMetadata, ServerNotebook, ServerObject,
    ServerPermissions, ServerWorkflow, UpdateCloudObjectResult,
};
use crate::drive::folders::FolderId;
use crate::drive::sharing::SharingAccessLevel;
use crate::notebooks::NotebookId;
use crate::server::cloud_objects::listener::ObjectUpdateMessage;
use crate::server::cloud_objects::update_manager::InitialLoadResponse;
use crate::server::ids::{ClientId, HashableId, ServerId, ServerIdAndType, SyncId, ToServerId};
use crate::server::server_api::object::{GuestIdentifier, ObjectClient};
use crate::server::sync_queue::SerializedModel;
use crate::workflows::WorkflowId;

/// The one row shape this client reads back from `object_metadata`.
#[cfg(feature = "local_fs")]
struct MetadataRow {
    object_type: String,
    revision_ts: Option<i64>,
    folder_id: Option<String>,
    is_welcome_object: bool,
    creator_uid: Option<String>,
    last_editor_uid: Option<String>,
}

/// Local stand-in for the Krust cloud-objects server. Stateless other than
/// the path to the app's SQLite database, which it reads (never writes) for
/// the few answers only the database has.
#[derive(Default)]
pub struct LocalObjectClient;

impl LocalObjectClient {
    pub fn new() -> Self {
        Self
    }

    /// Fabricates a server ID. Real ones are 22 opaque characters
    /// (`SERVER_ID_LENGTH` in `cloud_objects::ids`, private there — and
    /// `from_string_lossy` debug-panics on any other length); 22 hex chars
    /// of a v4 UUID (88 bits) is collision-proof for one machine.
    fn fabricate_server_id() -> ServerId {
        const SERVER_ID_LENGTH: usize = 22;
        let hex = Uuid::new_v4().simple().to_string();
        ServerId::from_string_lossy(&hex[..SERVER_ID_LENGTH])
    }

    fn now() -> ServerTimestamp {
        ServerTimestamp::new(Utc::now())
    }

    /// A fresh acknowledgement revision. Revisions are timestamps and the
    /// local client is the only "server", so now() is always the latest.
    fn ack_revision() -> RevisionAndLastEditor {
        RevisionAndLastEditor {
            revision: Self::now().into(),
            last_editor_uid: None,
        }
    }

    /// Owner-only permissions: no guests, no link sharing.
    fn private_permissions(owner: Owner) -> ServerPermissions {
        ServerPermissions {
            space: owner,
            guests: Vec::new(),
            anyone_link_sharing: None,
            permissions_last_updated_ts: Self::now(),
        }
    }

    fn created(request: CreateObjectRequest, id_type: ObjectIdType) -> CreateCloudObjectResult {
        CreateCloudObjectResult::Success {
            created_cloud_object: CreatedCloudObject {
                client_id: request.client_id,
                revision_and_editor: Self::ack_revision(),
                metadata_ts: Self::now(),
                server_id_and_type: ServerIdAndType {
                    id: Self::fabricate_server_id(),
                    id_type,
                },
                creator_uid: None,
                permissions: Self::private_permissions(request.owner),
            },
        }
    }

    /// Maps the `object_metadata.object_type` column (see
    /// [`ObjectType::sqlite_object_type_as_str`]) back to an ID type.
    /// Anything that isn't one of the three first-class types is a generic
    /// string object format string.
    #[cfg(feature = "local_fs")]
    fn object_id_type_from_column(object_type: &str) -> ObjectIdType {
        match object_type {
            "NOTEBOOK" => ObjectIdType::Notebook,
            "WORKFLOW" => ObjectIdType::Workflow,
            "FOLDER" => ObjectIdType::Folder,
            _ => ObjectIdType::GenericStringObject,
        }
    }

    /// Opens a fresh read-only connection to the app database. Cheap at
    /// Drive's call rates, and never contends with the app's single writer.
    #[cfg(feature = "local_fs")]
    fn open_read_connection() -> Result<diesel::SqliteConnection> {
        let path = crate::persistence::database_file_path_for_current_scope();
        crate::persistence::establish_ro_connection(&path.to_string_lossy())
    }

    /// The `server_id` column stores `"{Prefix}-{uid}"` (see
    /// `ServerId::sqlite_type_and_uid_hash`), and callers only have the bare
    /// uid, so a lookup matches all four possible prefixed forms.
    #[cfg(feature = "local_fs")]
    fn server_id_column_forms(uid: &str) -> [String; 4] {
        [
            ObjectIdType::Notebook,
            ObjectIdType::Workflow,
            ObjectIdType::Folder,
            ObjectIdType::GenericStringObject,
        ]
        .map(|id_type| format!("{}-{uid}", id_type.sqlite_prefix()))
    }

    #[cfg(feature = "local_fs")]
    fn read_metadata_row(conn: &mut diesel::SqliteConnection, uid: &str) -> Result<MetadataRow> {
        use diesel::prelude::*;
        use persistence::schema::object_metadata::dsl as m;

        let (object_type, revision_ts, folder_id, is_welcome_object, creator_uid, last_editor_uid) =
            m::object_metadata
                .filter(m::server_id.eq_any(Self::server_id_column_forms(uid)))
                .select((
                    m::object_type,
                    m::revision_ts,
                    m::folder_id,
                    m::is_welcome_object,
                    m::creator_uid,
                    m::last_editor_uid,
                ))
                .first::<(
                    String,
                    Option<i64>,
                    Option<String>,
                    bool,
                    Option<String>,
                    Option<String>,
                )>(conn)?;
        Ok(MetadataRow {
            object_type,
            revision_ts,
            folder_id,
            is_welcome_object,
            creator_uid,
            last_editor_uid,
        })
    }

    /// Builds the metadata echo that online-only metadata operations
    /// (untrash, grab/give-up edit access) expect back from the server.
    /// Consumers apply it via `update_from_new_metadata_ts`, which uses
    /// `current_editor_uid`, `trashed_ts`, `folder_id`, `creator_uid`, and
    /// the metadata timestamp; the rest is carried along for completeness.
    #[cfg(feature = "local_fs")]
    fn metadata_echo_from_row(
        id: ServerId,
        trashed_ts: Option<ServerTimestamp>,
        row: MetadataRow,
    ) -> ServerMetadata {
        let revision = row
            .revision_ts
            .and_then(|micros| Revision::from_unix_timestamp_micros(micros).ok())
            .unwrap_or_else(|| Self::now().into());
        ServerMetadata {
            uid: id,
            revision,
            metadata_last_updated_ts: Self::now(),
            trashed_ts,
            // The column stores the parent's sqlite hash ("Folder-{uid}", or
            // "Client-{uuid}" for a not-yet-acknowledged folder, which a
            // ServerId-backed FolderId can't represent).
            folder_id: row.folder_id.as_deref().and_then(FolderId::from_hash),
            is_welcome_object: row.is_welcome_object,
            creator_uid: row.creator_uid,
            last_editor_uid: row.last_editor_uid,
            // Single-user machine: nobody else ever holds the edit baton.
            current_editor_uid: None,
        }
    }

    #[cfg(feature = "local_fs")]
    fn metadata_echo(id: ServerId, trashed_ts: Option<ServerTimestamp>) -> Result<ServerMetadata> {
        let mut conn = Self::open_read_connection()?;
        let row = Self::read_metadata_row(&mut conn, &id.uid())?;
        Ok(Self::metadata_echo_from_row(id, trashed_ts, row))
    }

    #[cfg(not(feature = "local_fs"))]
    fn metadata_echo(
        _id: ServerId,
        _trashed_ts: Option<ServerTimestamp>,
    ) -> Result<ServerMetadata> {
        Err(anyhow!(
            "local object client requires local_fs for metadata reads"
        ))
    }

    /// Converts a metadata row's ID columns to a [`SyncId`], preferring the
    /// server ID. Mirrors `cloud_object_persistence::id_from_metadata`: the
    /// `server_id` column stores `"{Prefix}-{uid}"`, so the prefix is
    /// stripped first. Malformed rows are skipped rather than panicking
    /// (`ServerId::from_string_lossy` debug-panics on bad lengths).
    #[cfg(feature = "local_fs")]
    fn sync_id_from_columns(
        server_id: Option<String>,
        client_id: Option<String>,
    ) -> Option<SyncId> {
        use crate::server::ids::parse_sqlite_id_to_uid;

        match (server_id, client_id) {
            (Some(server_id), _) => {
                let parsed = parse_sqlite_id_to_uid(server_id.clone())
                    .ok()
                    .and_then(|uid| ServerId::try_from(uid.as_str()).ok());
                if parsed.is_none() {
                    log::warn!("Skipping malformed server id in object_metadata: {server_id}");
                }
                parsed.map(SyncId::ServerId)
            }
            (None, Some(client_id)) => ClientId::from_hash(&client_id).map(SyncId::ClientId),
            (None, None) => None,
        }
    }

    /// The server normally computes deletion cascades; locally we walk the
    /// folder tree in SQLite. The `folder_id` column stores the parent
    /// folder's sqlite hash — "Folder-{uid}" once acknowledged,
    /// "Client-{uuid}" before — so the frontier carries those hash forms.
    #[cfg(feature = "local_fs")]
    fn collect_descendants(
        conn: &mut diesel::SqliteConnection,
        seed: ServerId,
    ) -> Result<Vec<SyncId>> {
        use diesel::prelude::*;
        use persistence::schema::object_metadata::dsl as m;

        let mut deleted = vec![SyncId::ServerId(seed)];
        let mut frontier = vec![seed.sqlite_type_and_uid_hash(ObjectIdType::Folder)];
        while !frontier.is_empty() {
            let children = m::object_metadata
                .filter(m::folder_id.eq_any(&frontier))
                .select((m::server_id, m::client_id, m::object_type))
                .load::<(Option<String>, Option<String>, String)>(conn)?;
            frontier = children
                .iter()
                .filter(|(_, _, object_type)| {
                    Self::object_id_type_from_column(object_type) == ObjectIdType::Folder
                })
                // Both ID columns already store the sqlite hash forms that
                // children's `folder_id` references, so use them verbatim.
                .filter_map(|(server_id, client_id, _)| {
                    server_id.clone().or_else(|| client_id.clone())
                })
                .collect();
            deleted.extend(
                children
                    .into_iter()
                    .filter_map(|(server_id, client_id, _)| {
                        Self::sync_id_from_columns(server_id, client_id)
                    }),
            );
        }
        Ok(deleted)
    }

    /// Everything in the trash. The `owner` filter is intentionally absent:
    /// a local drive has exactly one (personal) space.
    #[cfg(feature = "local_fs")]
    fn collect_trashed(conn: &mut diesel::SqliteConnection) -> Result<Vec<SyncId>> {
        use diesel::prelude::*;
        use persistence::schema::object_metadata::dsl as m;

        let rows = m::object_metadata
            .filter(m::trashed_ts.is_not_null())
            .select((m::server_id, m::client_id))
            .load::<(Option<String>, Option<String>)>(conn)?;
        Ok(rows
            .into_iter()
            .filter_map(|(server_id, client_id)| Self::sync_id_from_columns(server_id, client_id))
            .collect())
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl ObjectClient for LocalObjectClient {
    async fn create_workflow(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        Ok(Self::created(request, ObjectIdType::Workflow))
    }

    async fn update_workflow(
        &self,
        _workflow_id: WorkflowId,
        _data: SerializedModel,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerWorkflow>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: Self::ack_revision(),
        })
    }

    async fn bulk_create_generic_string_objects(
        &self,
        owner: Owner,
        objects: &[BulkCreateGenericStringObjectsRequest],
    ) -> Result<BulkCreateCloudObjectResult> {
        Ok(BulkCreateCloudObjectResult::Success {
            created_cloud_objects: objects
                .iter()
                .map(|object| CreatedCloudObject {
                    client_id: object.id,
                    revision_and_editor: Self::ack_revision(),
                    metadata_ts: Self::now(),
                    server_id_and_type: ServerIdAndType {
                        id: Self::fabricate_server_id(),
                        id_type: ObjectIdType::GenericStringObject,
                    },
                    creator_uid: None,
                    permissions: Self::private_permissions(owner),
                })
                .collect(),
        })
    }

    async fn create_generic_string_object(
        &self,
        _format: GenericStringObjectFormat,
        _uniqueness_key: Option<GenericStringObjectUniqueKey>,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        // Uniqueness keys exist to dedupe across a user's devices; with a
        // single local store there is nothing to conflict with.
        Ok(Self::created(request, ObjectIdType::GenericStringObject))
    }

    async fn create_notebook(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        Ok(Self::created(request, ObjectIdType::Notebook))
    }

    async fn update_notebook(
        &self,
        _notebook_id: NotebookId,
        _title: Option<String>,
        _data: Option<SerializedModel>,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerNotebook>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: Self::ack_revision(),
        })
    }

    async fn create_folder(&self, request: CreateObjectRequest) -> Result<CreateCloudObjectResult> {
        Ok(Self::created(request, ObjectIdType::Folder))
    }

    async fn update_folder(
        &self,
        _folder_id: FolderId,
        _name: SerializedModel,
    ) -> Result<UpdateCloudObjectResult<ServerFolder>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: Self::ack_revision(),
        })
    }

    async fn update_generic_string_object(
        &self,
        _object_id: GenericStringObjectId,
        _model: SerializedModel,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<Box<dyn ServerObject>>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: Self::ack_revision(),
        })
    }

    async fn grab_notebook_edit_access(&self, notebook_id: NotebookId) -> Result<ServerMetadata> {
        Self::metadata_echo(notebook_id.to_server_id(), None)
    }

    async fn give_up_notebook_edit_access(
        &self,
        notebook_id: NotebookId,
    ) -> Result<ServerMetadata> {
        Self::metadata_echo(notebook_id.to_server_id(), None)
    }

    async fn get_krust_drive_updates(
        &self,
        message_sender: Sender<ObjectUpdateMessage>,
        stream_ready_sender: Sender<()>,
    ) -> Result<()> {
        // Nothing else edits the local store, so the update stream carries
        // no messages. Signal readiness, then stay silent forever — the
        // Listener treats a returned future as a disconnection and would
        // reconnect-loop.
        let _ = stream_ready_sender.send(()).await;
        let _keep_alive = message_sender;
        std::future::pending::<Result<()>>().await
    }

    async fn fetch_changed_objects(
        &self,
        _objects_to_update: ObjectsToUpdate,
        _force_refresh: bool,
    ) -> Result<InitialLoadResponse> {
        // SQLite is the source of truth and CloudModel already loaded it at
        // startup; there is never anything new "server-side".
        Ok(InitialLoadResponse::default())
    }

    async fn fetch_single_cloud_object(&self, _id: ServerId) -> Result<GetCloudObjectResponse> {
        Err(anyhow!(
            "shared cloud objects are not available without a server"
        ))
    }

    async fn transfer_notebook_owner(
        &self,
        _notebook_id: NotebookId,
        _owner: Owner,
    ) -> Result<bool> {
        Err(anyhow!("ownership transfer is not available locally"))
    }

    async fn transfer_workflow_owner(
        &self,
        _workflow_id: WorkflowId,
        _owner: Owner,
    ) -> Result<bool> {
        Err(anyhow!("ownership transfer is not available locally"))
    }

    async fn transfer_generic_string_object_owner(
        &self,
        _object_id: GenericStringObjectId,
        _owner: Owner,
    ) -> Result<bool> {
        Err(anyhow!("ownership transfer is not available locally"))
    }

    async fn trash_object(&self, _id: ServerId) -> Result<bool> {
        // The UpdateManager already marked the object trashed locally and
        // persists it on this acknowledgement.
        Ok(true)
    }

    async fn untrash_object(&self, id: ServerId) -> Result<ObjectMetadataUpdateResult> {
        // Untrash is the one flow that is *not* optimistic: the consumer
        // waits for the server's metadata echo and applies it, so answer
        // with the object's real metadata minus the trash timestamp.
        match Self::metadata_echo(id, None) {
            Ok(metadata) => Ok(ObjectMetadataUpdateResult::Success {
                metadata: Box::new(metadata),
            }),
            Err(err) => {
                log::warn!("Failed to read metadata for untrash: {err}");
                Ok(ObjectMetadataUpdateResult::Failure)
            }
        }
    }

    async fn delete_object(&self, id: ServerId) -> Result<ObjectDeleteResult> {
        #[cfg(feature = "local_fs")]
        let deleted_ids = Self::open_read_connection()
            .and_then(|mut conn| Self::collect_descendants(&mut conn, id))
            .unwrap_or_else(|err| {
                log::warn!("Failed to collect folder descendants for delete: {err}");
                // Deleting just the requested object beats failing outright.
                vec![SyncId::ServerId(id)]
            });
        #[cfg(not(feature = "local_fs"))]
        let deleted_ids = vec![SyncId::ServerId(id)];

        Ok(ObjectDeleteResult::Success { deleted_ids })
    }

    async fn empty_trash(&self, _owner: Owner) -> Result<ObjectDeleteResult> {
        #[cfg(feature = "local_fs")]
        match Self::open_read_connection().and_then(|mut conn| Self::collect_trashed(&mut conn)) {
            Ok(deleted_ids) => Ok(ObjectDeleteResult::Success { deleted_ids }),
            Err(err) => {
                log::warn!("Failed to list trashed objects: {err}");
                Ok(ObjectDeleteResult::Failure)
            }
        }
        #[cfg(not(feature = "local_fs"))]
        Ok(ObjectDeleteResult::Failure)
    }

    async fn move_object(
        &self,
        _id: ServerId,
        _folder_id: Option<FolderId>,
        _owner: Owner,
        _object_type: ObjectType,
    ) -> Result<bool> {
        Ok(true)
    }

    #[cfg(not(feature = "local_fs"))]
    async fn record_object_action(
        &self,
        _id: ServerId,
        _action_type: ObjectActionType,
        _timestamp: DateTime<Utc>,
        _data: Option<String>,
    ) -> Result<ObjectActionHistory> {
        Err(anyhow!(
            "local object client requires local_fs to record actions"
        ))
    }

    #[cfg(feature = "local_fs")]
    async fn record_object_action(
        &self,
        id: ServerId,
        action_type: ObjectActionType,
        timestamp: DateTime<Utc>,
        data: Option<String>,
    ) -> Result<ObjectActionHistory> {
        // The hashed sqlite ID needs the object's type prefix, which only
        // the database knows from a bare server ID.
        let mut conn = Self::open_read_connection()?;
        let row = Self::read_metadata_row(&mut conn, &id.uid())?;
        let id_type = Self::object_id_type_from_column(&row.object_type);
        let processed_at = Utc::now();
        Ok(ObjectActionHistory {
            uid: id.uid(),
            hashed_sqlite_id: id.sqlite_type_and_uid_hash(id_type),
            latest_processed_at_timestamp: processed_at,
            actions: vec![ObjectAction {
                action_type,
                uid: id.uid(),
                hashed_sqlite_id: id.sqlite_type_and_uid_hash(id_type),
                action_subtype: ObjectActionSubtype::SingleAction {
                    timestamp,
                    processed_at_timestamp: Some(processed_at),
                    data,
                    pending: false,
                },
            }],
        })
    }

    async fn leave_object(&self, _id: ServerId) -> Result<ObjectDeleteResult> {
        Err(anyhow!("shared objects are not available locally"))
    }

    async fn set_object_link_permissions(
        &self,
        _object_id: ServerId,
        _access_level: SharingAccessLevel,
    ) -> Result<ObjectPermissionUpdateResult> {
        Err(anyhow!("link sharing is not available locally"))
    }

    async fn remove_object_link_permissions(
        &self,
        _object_id: ServerId,
    ) -> Result<ObjectPermissionUpdateResult> {
        Err(anyhow!("link sharing is not available locally"))
    }

    async fn add_object_guests(
        &self,
        _object_id: ServerId,
        _guest_emails: Vec<String>,
        _access_level: AccessLevel,
    ) -> Result<ObjectPermissionsUpdateData> {
        Err(anyhow!("guest sharing is not available locally"))
    }

    async fn update_object_guests(
        &self,
        _object_id: ServerId,
        _guest_emails: Vec<String>,
        _access_level: AccessLevel,
    ) -> Result<ServerPermissions> {
        Err(anyhow!("guest sharing is not available locally"))
    }

    async fn remove_object_guest(
        &self,
        _object_id: ServerId,
        _guest: GuestIdentifier,
    ) -> Result<ServerPermissions> {
        Err(anyhow!("guest sharing is not available locally"))
    }

    async fn fetch_environment_last_task_run_timestamps(
        &self,
    ) -> Result<HashMap<String, DateTime<Utc>>> {
        Ok(HashMap::new())
    }
}

#[cfg(all(test, feature = "local_fs"))]
#[path = "local_object_client_tests.rs"]
mod tests;
