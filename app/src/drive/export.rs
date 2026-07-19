use std::collections::hash_map::{Entry, OccupiedEntry};
use std::collections::HashMap;
#[cfg(feature = "local_fs")]
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[cfg(feature = "local_fs")]
use aho_corasick::{AhoCorasick, MatchKind};
#[cfg(feature = "local_fs")]
use anyhow::{anyhow, Context};
#[cfg(feature = "local_fs")]
use futures::AsyncWriteExt;
use warp_util::path::ShellFamily;
use warpui::platform::file_picker::FilePickerError;
use warpui::platform::{FilePickerConfiguration, OperatingSystem};
use warpui::r#async::SpawnedFutureHandle;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity, WindowId};

use super::CloudObjectTypeAndId;
use crate::cloud_object::model::persistence::CloudModel;
#[cfg(feature = "local_fs")]
use crate::cloud_object::CloudObjectLocation;
use crate::cloud_object::Space;
use crate::safe_warn;
#[cfg(feature = "local_fs")]
use crate::server::ids::SyncId;
use crate::view_components::DismissibleToast;
use crate::workspace::{active_terminal_in_window, ToastStack};
#[cfg(feature = "local_fs")]
use crate::{
    notebooks::export_notebook, server::cloud_objects::update_manager::get_duplicate_object_name,
    view_components::ToastLink, workflows::export_workflow::export_serialize,
    workspace::WorkspaceAction,
};

/// Singleton model for exporting from Warp Drive.
pub struct ExportManager {
    exports: HashMap<ExportId, Export>,
}

/// Identifier for an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExportId(CloudObjectTypeAndId, Space);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEvent {
    /// Export of this item was canceled.
    Canceled(ExportId),
    /// Export of this item failed.
    Failed {
        /// The overall export ID.
        id: ExportId,
    },
    /// Export completed.
    #[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
    Completed { id: ExportId, path: PathBuf },
}

/// A single Warp Drive export.
struct Export {
    /// The ID of the window that started this export, for showing toasts.
    window_id: WindowId,
    state: State,
    // Whether this is a bulk export.
    is_bulk: bool,
    /// Fork: directory relative to the picked location this object should
    /// be written into. `Some` for objects that came from expanding a
    /// folder export, mirroring the Drive folder structure on disk.
    subdir: Option<PathBuf>,
}

enum State {
    /// The user is picking where to export to.
    ChoosingLocation,
    Exporting(SpawnedFutureHandle),
}

/// # Flow
/// The overall flow for export is asynchronous, and requires user input at the beginning.
/// 1. An entrypoint calls [`ExportManager::export`] to start a new export.
///    This initializes some state and opens a file picker.
/// 2. The user chooses a directory or cancels, calling [`ExportManager::handle_files_picked`].
///    If they canceled, the export ends. Otherwise [`ExportManager::run_export`] begins exporting
///    individual objects.
/// 3. Each object to export is processed by [`ExportManager::export_one`], which serializes the
///    object and then asynchronously writes it to disk using [`write_object`].
/// 4. Once writing an object finishes, the result is handled by
///    [`ExportManager::handle_object_export`]. If the export is done, it emits an
///    [`ExportEvent::Completed`] event. If it failed, it emits an [`ExportEvent::Failed`].
impl ExportManager {
    pub fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            exports: Default::default(),
        }
    }

    /// Export a list of objects.
    ///
    /// Fork: folders are expanded to their exportable descendants, and the
    /// exported files mirror the Drive folder structure as directories.
    pub fn export(
        &mut self,
        window_id: WindowId,
        objects: &[CloudObjectTypeAndId],
        ctx: &mut ModelContext<Self>,
    ) {
        let shell_family =
            active_terminal_in_window(window_id, ctx, |terminal, ctx| terminal.shell_family(ctx))
                .unwrap_or_else(|| OperatingSystem::get().default_shell_family());

        // Expand folders before the exportability filter below — a folder
        // isn't an exportable object itself; it becomes directories.
        let mut targets: Vec<(CloudObjectTypeAndId, Option<PathBuf>)> = Vec::new();
        for object in objects {
            match object {
                #[cfg(feature = "local_fs")]
                CloudObjectTypeAndId::Folder(folder_id) => {
                    targets.extend(
                        Self::folder_export_targets(*folder_id, ctx)
                            .into_iter()
                            .map(|(object, subdir)| (object, Some(subdir))),
                    );
                }
                #[cfg(not(feature = "local_fs"))]
                CloudObjectTypeAndId::Folder(_) => {
                    log::warn!("Folder export requires a local filesystem");
                }
                _ => targets.push((*object, None)),
            }
        }

        if targets.is_empty() {
            // Everything expanded away (e.g. an empty folder): tell the user
            // instead of opening a file picker that would do nothing.
            ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                toast_stack.add_ephemeral_toast(
                    DismissibleToast::error("Nothing to export".to_string()),
                    window_id,
                    ctx,
                );
            });
            return;
        }

        let is_bulk = targets.len() > 1;
        let mut ids = Vec::new();
        for (object, subdir) in targets {
            match CloudModel::as_ref(ctx).get_by_uid(&object.uid()) {
                None => log::warn!("Tried to export unknown object {object:?}"),
                Some(obj) if !obj.can_export() => {
                    log::warn!("Tried to export un-exportable object {object:?}")
                }
                Some(cloud_object) => {
                    let id = ExportId(object, cloud_object.space(ctx));
                    ids.push(id);
                    match self.exports.entry(id) {
                        Entry::Occupied(_) => {
                            log::info!("Object {object:?} is already being exported")
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(Export::new(window_id, is_bulk, subdir));
                        }
                    }
                }
            }
        }
        if ids.is_empty() {
            return;
        }
        ctx.open_file_picker(
            move |result, app| {
                Self::handle(app).update(app, |me, ctx| {
                    me.handle_files_picked(ids, result, shell_family, ctx);
                });
            },
            FilePickerConfiguration::new().folders_only(),
        );
    }

    /// Walks a folder's active descendants, mapping each exportable object
    /// to the directory (relative to the picked export location) that
    /// mirrors its position in the Drive folder tree. Sub-folders become
    /// nested directories; non-exportable objects are skipped.
    #[cfg(feature = "local_fs")]
    fn folder_export_targets(
        root_id: SyncId,
        ctx: &ModelContext<Self>,
    ) -> Vec<(CloudObjectTypeAndId, PathBuf)> {
        let cloud_model = CloudModel::as_ref(ctx);
        let root_name = cloud_model
            .get_by_uid(&root_id.uid())
            .map(|folder| folder.display_name())
            .unwrap_or_default();

        let mut targets = Vec::new();
        let mut stack = vec![(root_id, PathBuf::from(export_dir_name(&root_name)))];
        while let Some((folder_id, dir)) = stack.pop() {
            for child in cloud_model.active_cloud_objects_in_location_without_descendents(
                CloudObjectLocation::Folder(folder_id),
                ctx,
            ) {
                let type_and_id = child.cloud_object_type_and_id();
                if let CloudObjectTypeAndId::Folder(child_folder_id) = type_and_id {
                    stack.push((
                        child_folder_id,
                        dir.join(export_dir_name(&child.display_name())),
                    ));
                } else if child.can_export() {
                    targets.push((type_and_id, dir.clone()));
                }
            }
        }
        targets
    }

    /// Handle the file picker selection.
    fn handle_files_picked(
        &mut self,
        ids: Vec<ExportId>,
        result: Result<Vec<String>, FilePickerError>,
        shell_family: ShellFamily,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok(mut paths) => {
                match paths.pop() {
                    Some(path) => {
                        let path = PathBuf::from(path);
                        for id in ids {
                            self.run_export(id, &path, shell_family, ctx);
                        }
                    }
                    None => {
                        // User cancelled
                        for id in ids {
                            self.cancel(id, ctx);
                        }
                    }
                }
            }
            Err(err) => {
                if let Some(export) = ids.first().and_then(|id| self.exports.get(id)) {
                    let window_id = export.window_id;
                    ToastStack::handle(ctx).update(ctx, move |toast_stack, ctx| {
                        let toast = DismissibleToast::error(format!("{err}"));
                        toast_stack.add_ephemeral_toast(toast, window_id, ctx);
                    });
                }
                for id in ids {
                    self.cancel(id, ctx);
                }
            }
        }
    }

    /// Begin exporting into the given directory.
    fn run_export(
        &mut self,
        id: ExportId,
        path: &Path,
        shell_family: ShellFamily,
        ctx: &mut ModelContext<Self>,
    ) {
        match self.exports.entry(id) {
            Entry::Occupied(mut export) => match export.get().state {
                State::ChoosingLocation => {
                    log::debug!("Exporting {id:?} to {}", path.display());
                    match Self::export_one(
                        id,
                        export.get().is_bulk,
                        export.get().subdir.clone(),
                        path,
                        id.0,
                        shell_family,
                        ctx,
                    ) {
                        Ok(handle) => {
                            export.get_mut().state = State::Exporting(handle);
                        }
                        Err(ref err) => Self::handle_failure(export, err, ctx),
                    }
                }
                State::Exporting(_) => {
                    log::warn!("Tried to restart in-progress export of {id:?}");
                }
            },
            Entry::Vacant(_) => {
                log::warn!("Tried to start unknown export for {id:?}");
            }
        }
    }

    /// Handle an object's export finishing.
    #[cfg(feature = "local_fs")]
    fn handle_object_export(
        &mut self,
        id: ExportId,
        object: CloudObjectTypeAndId,
        path: anyhow::Result<PathBuf>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (is_bulk, window_id, open_dir) = match self.exports.entry(id) {
            Entry::Occupied(export) => match path {
                Ok(ref path) => {
                    let (is_bulk, window_id) = (export.get().is_bulk, export.get().window_id);
                    // For folder-derived exports, the directory worth opening
                    // from the completion toast is the exported folder's
                    // root, not the (possibly nested) subdirectory the last
                    // file happened to land in.
                    let open_dir = match &export.get().subdir {
                        Some(subdir) => path
                            .ancestors()
                            .nth(subdir.components().count())
                            .map(Path::to_path_buf),
                        None => path.parent().map(Path::to_path_buf),
                    };
                    log::debug!("Exported {object:?} to {} successfully", path.display());
                    Self::handle_completion(export, path.clone(), ctx);
                    (is_bulk, window_id, open_dir)
                }
                Err(ref err) => {
                    let (is_bulk, window_id) = (export.get().is_bulk, export.get().window_id);
                    Self::handle_failure(export, err, ctx);
                    (is_bulk, window_id, None)
                }
            },
            Entry::Vacant(_) => {
                log::warn!("Received update for unknown export {id:?}");
                return;
            }
        };
        if is_bulk && self.exports.is_empty() {
            ToastStack::handle(ctx).update(ctx, move |toast_stack, ctx| {
                let link_label = if cfg!(target_os = "macos") {
                    "Open in Finder"
                } else {
                    "Open in folder"
                };

                let mut toast_link = ToastLink::new(link_label.to_string());
                if let Some(root_dir) = open_dir {
                    toast_link = toast_link
                        .with_onclick_action(WorkspaceAction::OpenInExplorer { path: root_dir });
                }
                toast_stack.add_ephemeral_toast(
                    DismissibleToast::success("Finished exporting objects".to_string())
                        .with_link(toast_link),
                    window_id,
                    ctx,
                );
            });
        }
    }

    /// Drive export of a single object.
    #[cfg(feature = "local_fs")]
    fn export_one(
        id: ExportId,
        is_bulk: bool,
        subdir: Option<PathBuf>,
        parent_path: &Path,
        object: CloudObjectTypeAndId,
        shell_family: ShellFamily,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<SpawnedFutureHandle> {
        let cloud_model = CloudModel::as_ref(ctx);
        let (name, extension, data) = match object {
            CloudObjectTypeAndId::Workflow(workflow_id) => {
                let workflow = cloud_model
                    .get_workflow(&workflow_id)
                    .ok_or_else(|| anyhow!("no workflow for {workflow_id}"))?;

                let mut serializer = serde_yaml::Serializer::new(Vec::new());
                export_serialize(&workflow.model().data, &mut serializer, ctx)?;
                let data = serializer.into_inner();

                (workflow.model().data.name().to_owned(), "yaml", data)
            }
            CloudObjectTypeAndId::Notebook(notebook_id) => {
                let notebook = cloud_model
                    .get_notebook(&notebook_id)
                    .ok_or_else(|| anyhow!("no notebook for {notebook_id}"))?;
                let internal_data = &notebook.model().data;
                // If we're unable to translate the Markdown for export, fall back to the original
                // text.
                let data = export_notebook(internal_data, ctx)
                    .unwrap_or_else(|_| internal_data.clone())
                    .into_bytes();
                (notebook.model().title.clone(), "md", data)
            }
            CloudObjectTypeAndId::GenericStringObject { object_type, id } => {
                if let Some(env_var_collection) = cloud_model.get_env_var_collection(&id) {
                    let env_var_collection_model = env_var_collection.model();

                    let exported_variables = env_var_collection_model
                        .string_model
                        .export_variables("\n", shell_family)
                        .into_bytes();

                    (
                        env_var_collection_model
                            .string_model
                            .title
                            .clone()
                            .unwrap_or_default(),
                        "env",
                        exported_variables,
                    )
                } else {
                    anyhow::bail!("exporting {object_type:?} not yet supported")
                }
            }
            other => {
                anyhow::bail!("exporting {other:?} not yet supported")
            }
        };

        let name = if name.is_empty() {
            "Untitled".to_string()
        } else {
            safe_filename(&name)
        };

        // Folder-derived exports mirror the Drive folder tree; plain bulk
        // exports keep the upstream behavior of a space-named directory.
        let path = match &subdir {
            Some(subdir) => parent_path.join(subdir),
            None if is_bulk => parent_path.join(safe_filename(&id.1.name(ctx))),
            None => parent_path.to_path_buf(),
        };
        let create_dirs = is_bulk || subdir.is_some();

        Ok(ctx.spawn(
            async move { write_object(path, create_dirs, name, extension, data).await },
            move |me, result, ctx| {
                me.handle_object_export(id, object, result, ctx);
            },
        ))
    }

    #[cfg(not(feature = "local_fs"))]
    fn export_one(
        _id: ExportId,
        _is_bulk: bool,
        _subdir: Option<PathBuf>,
        _parent_path: &Path,
        _object: CloudObjectTypeAndId,
        _shell_family: ShellFamily,
        _ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<SpawnedFutureHandle> {
        anyhow::bail!("export not supported without a local filesystem")
    }

    /// Cancel an export.
    fn cancel(&mut self, id: ExportId, ctx: &mut ModelContext<Self>) {
        if self.exports.remove(&id).is_some() {
            ctx.emit(ExportEvent::Canceled(id));
        }
    }

    /// Handle an error exporting an object.
    fn handle_failure(
        export: OccupiedEntry<ExportId, Export>,
        error: &anyhow::Error,
        ctx: &mut ModelContext<Self>,
    ) {
        let id = *export.key();
        // Don't send the error to Sentry, since it likely includes a user file path and their Warp
        // Drive object name. Also don't report this as an error, since the most likely failure
        // reason is an I/O issue on the user's machine (like being out of disk space, or exporting
        // to a directory they can't write to).
        safe_warn!(
            safe: ("Exporting {id:?} failed"),
            full: ("Exporting {id:?} failed: {error:#}")
        );
        ctx.emit(ExportEvent::Failed { id: *export.key() });
        let window_id = export.remove().window_id;
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            let message = match id.display_name(ctx) {
                Some(name) => format!("Failed to export {name}"),
                None => "Export failed".to_string(),
            };
            toast_stack.add_persistent_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }

    /// Handle the last object in an export completing successfully.
    #[cfg(feature = "local_fs")]
    fn handle_completion(
        export: OccupiedEntry<ExportId, Export>,
        root_path: PathBuf,
        ctx: &mut ModelContext<Self>,
    ) {
        ctx.emit(ExportEvent::Completed {
            id: *export.key(),
            path: root_path.clone(),
        });
        if !export.get().is_bulk {
            ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                let message = match export.key().display_name(ctx) {
                    Some(name) => format!("Exported {name}"),
                    None => "Exported object".to_string(),
                };

                let link_label = if cfg!(target_os = "macos") {
                    "Open in Finder"
                } else {
                    "Open in folder"
                };

                toast_stack.add_ephemeral_toast(
                    DismissibleToast::success(message).with_link(
                        ToastLink::new(link_label.to_string()).with_onclick_action(
                            WorkspaceAction::OpenInExplorer { path: root_path },
                        ),
                    ),
                    export.get().window_id,
                    ctx,
                );
            });
        }
        export.remove();
    }
}

impl Entity for ExportManager {
    type Event = ExportEvent;
}

impl SingletonEntity for ExportManager {}

impl Export {
    fn new(window_id: WindowId, is_bulk: bool, subdir: Option<PathBuf>) -> Self {
        Self {
            is_bulk,
            state: State::ChoosingLocation,
            window_id,
            subdir,
        }
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        if let State::Exporting(handle) = &self.state {
            handle.abort();
        }
    }
}

impl ExportId {
    /// Display name for the root object being exported.
    pub fn display_name(self, ctx: &AppContext) -> Option<String> {
        CloudModel::as_ref(ctx)
            .get_by_uid(&self.0.uid())
            .map(|object| {
                let mut name = object.display_name();
                if name.is_empty() {
                    name.push_str("Untitled")
                }
                name
            })
    }
}

/// Write an object's exported representation to disk.
#[cfg(feature = "local_fs")]
async fn write_object(
    parent_path: PathBuf,
    create_dirs: bool,
    object_name: String,
    extension: &str,
    object_data: Vec<u8>,
) -> anyhow::Result<PathBuf> {
    use anyhow::bail;

    if object_name.is_empty() {
        // This should be handled in `export_one`, but do a final check here before writing
        // anything to disk.
        bail!("Cannot export unnamed object");
    }

    // Create the full path if it doesn't exist
    if create_dirs {
        async_fs::create_dir_all(&parent_path)
            .await
            .with_context(|| format!("could not create directory {}", parent_path.display()))?;
    }

    let mut current_name = object_name;
    let mut open_options = async_fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    loop {
        let mut current_path = parent_path.join(&current_name);
        current_path.set_extension(extension);
        let file = open_options.open(&current_path).await;
        match file {
            Ok(mut file) => {
                file.write_all(&object_data)
                    .await
                    .with_context(|| format!("could not export to {}", current_path.display()))?;
                file.flush().await?;
                return Ok(current_path);
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                current_name = get_duplicate_object_name(&current_name);
            }
            Err(err) => {
                return Err(anyhow::Error::new(err)
                    .context(format!("could not create {}", current_path.display())))
            }
        }
    }
}

#[cfg(feature = "local_fs")]
lazy_static::lazy_static! {
    /// Matcher for characters which are forbidden in filenames.
    static ref FORBIDDEN_FILENAME_PATTERNS: AhoCorasick = make_forbidden_filenames_matcher();
}

/// This is a helper for [`safe_filename`], which constructs a cached [`AhoCorasick`] matcher to
/// replace forbidden filename characters.
#[cfg(feature = "local_fs")]
fn make_forbidden_filenames_matcher() -> AhoCorasick {
    // NTFS (Windows) disallows ASCII control characters in path names.
    let ascii_control = 0x00..0x1f;
    // These characters are disallowed by UNIX filesystems, APFS or HFS+ (macOS), or NTFS.
    let forbidden = [b'/', b':', b'#', b'*', b'<', b'>', b'?', b'\\', b'|'];

    let patterns = ascii_control.chain(forbidden).map(|ch| [ch]);
    AhoCorasick::builder()
        .match_kind(MatchKind::LeftmostFirst)
        .build(patterns)
        .expect("Path patterns should compile")
}

/// Directory name for an exported Drive folder: its display name made
/// filesystem-safe, or "Untitled" when the folder has no name.
#[cfg(feature = "local_fs")]
fn export_dir_name(name: &str) -> String {
    if name.trim().is_empty() {
        "Untitled".to_string()
    } else {
        safe_filename(name)
    }
}

/// Replaces characters that are not allowed in a path name. This is _not_ escaping - disallowed
/// characters cannot be escaped in a path.
///
/// See [Comparison of filename limitations](https://en.wikipedia.org/wiki/Filename#Comparison_of_filename_limitations).
#[cfg(feature = "local_fs")]
pub fn safe_filename(filename: &str) -> String {
    let mut result = String::new();
    FORBIDDEN_FILENAME_PATTERNS.replace_all_with(filename, &mut result, |_, _, dst| {
        // This replaces all forbidden characters with a `_`. We could use the match arguments to
        // replace specific characters with something closer to their original semantics.
        dst.push('_');
        true
    });
    result
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
