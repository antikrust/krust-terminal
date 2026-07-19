use std::collections::HashSet;

use diesel::prelude::*;
use diesel_migrations::MigrationHarness;
use persistence::model::NewObjectMetadata;
use persistence::schema::object_metadata::dsl as m;

use super::LocalObjectClient;
use crate::server::ids::{ClientId, ServerId, SyncId, ToServerId};

fn test_conn() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("in-memory sqlite");
    conn.run_pending_migrations(persistence::MIGRATIONS)
        .expect("migrations run");
    conn
}

/// A well-formed 22-char server uid made of one repeated character.
fn uid(fill: char) -> String {
    std::iter::repeat_n(fill, 22).collect()
}

fn sid(fill: char) -> ServerId {
    ServerId::try_from(uid(fill).as_str()).expect("valid server id")
}

struct RowSpec {
    server_id: Option<String>,
    client_id: Option<String>,
    object_type: &'static str,
    folder_id: Option<String>,
    trashed_ts: Option<i64>,
    revision_ts: Option<i64>,
}

impl Default for RowSpec {
    fn default() -> Self {
        Self {
            server_id: None,
            client_id: None,
            object_type: "WORKFLOW",
            folder_id: None,
            trashed_ts: None,
            revision_ts: None,
        }
    }
}

fn insert_row(conn: &mut SqliteConnection, spec: RowSpec) {
    diesel::insert_into(m::object_metadata)
        .values(NewObjectMetadata {
            is_pending: false,
            object_type: spec.object_type.to_string(),
            revision_ts: spec.revision_ts,
            server_id: spec.server_id,
            client_id: spec.client_id,
            shareable_object_id: 0,
            author_id: None,
            retry_count: 0,
            metadata_last_updated_ts: None,
            trashed_ts: spec.trashed_ts,
            folder_id: spec.folder_id,
            is_welcome_object: false,
            creator_uid: Some("creator".to_string()),
            last_editor_uid: None,
            current_editor: None,
        })
        .execute(conn)
        .expect("insert object_metadata row");
}

fn collected_uids(ids: Vec<SyncId>) -> HashSet<String> {
    ids.into_iter().map(|id| id.uid()).collect()
}

#[test]
fn collects_folder_descendants_including_nested_and_unsynced_children() {
    let mut conn = test_conn();
    let client_only = ClientId::new();

    // Folder a
    //  ├── notebook b
    //  ├── folder c
    //  │     └── workflow d
    //  └── unsynced env var collection (client id only)
    // notebook e sits outside the tree.
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Folder-{}", uid('a'))),
            object_type: "FOLDER",
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Notebook-{}", uid('b'))),
            object_type: "NOTEBOOK",
            folder_id: Some(format!("Folder-{}", uid('a'))),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Folder-{}", uid('c'))),
            object_type: "FOLDER",
            folder_id: Some(format!("Folder-{}", uid('a'))),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Workflow-{}", uid('d'))),
            object_type: "WORKFLOW",
            folder_id: Some(format!("Folder-{}", uid('c'))),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            client_id: Some(client_only.to_string()),
            object_type: "JsonEnvVarCollection",
            folder_id: Some(format!("Folder-{}", uid('a'))),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Notebook-{}", uid('e'))),
            object_type: "NOTEBOOK",
            ..Default::default()
        },
    );

    let deleted = LocalObjectClient::collect_descendants(&mut conn, sid('a'))
        .expect("descendant walk succeeds");

    let expected: HashSet<String> = [
        uid('a'),
        uid('b'),
        uid('c'),
        uid('d'),
        client_only.to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(collected_uids(deleted), expected);
}

#[test]
fn collect_trashed_returns_only_trashed_rows() {
    let mut conn = test_conn();
    let trashed_client_only = ClientId::new();

    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Workflow-{}", uid('a'))),
            trashed_ts: Some(1_000),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            client_id: Some(trashed_client_only.to_string()),
            trashed_ts: Some(2_000),
            ..Default::default()
        },
    );
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Notebook-{}", uid('b'))),
            ..Default::default()
        },
    );

    let trashed = LocalObjectClient::collect_trashed(&mut conn).expect("trash listing succeeds");

    let expected: HashSet<String> = [uid('a'), trashed_client_only.to_string()]
        .into_iter()
        .collect();
    assert_eq!(collected_uids(trashed), expected);
}

#[test]
fn read_metadata_row_finds_rows_by_bare_uid_for_any_type_prefix() {
    let mut conn = test_conn();
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Notebook-{}", uid('a'))),
            object_type: "NOTEBOOK",
            ..Default::default()
        },
    );

    let row =
        LocalObjectClient::read_metadata_row(&mut conn, &uid('a')).expect("row found by bare uid");
    assert_eq!(row.object_type, "NOTEBOOK");
    assert_eq!(row.creator_uid.as_deref(), Some("creator"));
}

#[test]
fn metadata_echo_maps_folder_hash_and_keeps_revision() {
    let mut conn = test_conn();
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Notebook-{}", uid('a'))),
            object_type: "NOTEBOOK",
            folder_id: Some(format!("Folder-{}", uid('f'))),
            revision_ts: Some(123_456),
            ..Default::default()
        },
    );

    let row = LocalObjectClient::read_metadata_row(&mut conn, &uid('a')).expect("row found");
    let echo = LocalObjectClient::metadata_echo_from_row(sid('a'), None, row);

    assert_eq!(echo.trashed_ts, None);
    assert_eq!(
        echo.folder_id.map(|folder| folder.to_server_id().uid()),
        Some(uid('f'))
    );
    assert_eq!(echo.revision.timestamp_micros(), 123_456);
    assert_eq!(echo.creator_uid.as_deref(), Some("creator"));
    assert_eq!(echo.current_editor_uid, None);
}

#[test]
fn metadata_echo_drops_client_only_parent_folders() {
    let mut conn = test_conn();
    insert_row(
        &mut conn,
        RowSpec {
            server_id: Some(format!("Workflow-{}", uid('a'))),
            folder_id: Some(ClientId::new().to_string()),
            ..Default::default()
        },
    );

    let row = LocalObjectClient::read_metadata_row(&mut conn, &uid('a')).expect("row found");
    let echo = LocalObjectClient::metadata_echo_from_row(sid('a'), None, row);
    assert_eq!(echo.folder_id, None);
}
