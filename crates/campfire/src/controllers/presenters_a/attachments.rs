//! `has_one_attached` as `User::Avatar` (`:avatar`) and `Account` (`:logo`) use it, over
//! `campfire_storage` (reference/app/models/user/avatar.rb, account.rb, and Active Storage's
//! `Attached::Changes::CreateOne` / `Attachment`).
//!
//! Assigning an uploaded file and saving the record, in the record's transaction:
//! the old attachment is destroyed first (`has_one ... dependent: :destroy` replacing its target;
//! its blob is purged after commit, `dependent: :purge_later`), then the new blob and attachment
//! rows are inserted, and each attachment change touches the record
//! (`belongs_to :record, touch: true`). After commit the file is uploaded and, since a fresh blob
//! isn't analyzed, `ActiveStorage::AnalyzeJob` runs later (analysis touches the record again).

use campfire_db::{CachedStatements, Connection, Event, Tx};
use campfire_kit::{Error, Param, Result};
use campfire_storage::{Blob, Filename, NewBlob, Variation};

use crate::app::App;

/// The storage service `config/storage.yml` names for production.
const SERVICE_NAME: &str = "local";

/// An uploaded file, read into memory (`ActionDispatch::Http::UploadedFile`).
#[derive(Debug, Clone)]
pub struct Upload {
    pub data: Vec<u8>,
    pub filename: String,
    pub content_type: Option<String>,
}

impl Upload {
    /// The upload in `param`, if it is one. `""` and nil mean "no change" to the controllers
    /// here (`params.permit(...).compact` / `avatar=` with nil or "" deletes, see [`Assignment`]).
    pub fn from_param(param: Option<&Param>) -> Result<Option<Upload>> {
        let Some(file) = param.and_then(Param::as_file) else { return Ok(None) };
        Ok(Some(Upload {
            data: file.read()?,
            filename: file.original_filename.clone(),
            content_type: file.content_type.clone(),
        }))
    }
}

/// What `record.avatar = value` does with a permitted param value.
#[derive(Debug, Clone)]
pub enum Assignment {
    /// The key wasn't given.
    Unchanged,
    /// `nil` or `""`: `Attached::Changes::DeleteOne` (the attachment is destroyed on save).
    Delete,
    /// An uploaded file: `Attached::Changes::CreateOne`.
    Create(Upload),
    /// Anything else (e.g. a plain string that isn't a signed blob id): Rails raises.
    Invalid,
}

impl Assignment {
    pub fn from_params(params: &campfire_kit::ParamMap, key: &str) -> Result<Assignment> {
        if !params.contains_key(key) {
            return Ok(Assignment::Unchanged);
        }
        match params.get(key) {
            None => Ok(Assignment::Delete),
            Some(param) if param.is_null() || param.as_str() == Some("") => Ok(Assignment::Delete),
            Some(param) if param.as_file().is_some() => Ok(Assignment::Create(Upload::from_param(Some(param))?.expect("a file"))),
            Some(_) => Ok(Assignment::Invalid),
        }
    }
}

/// A blob inserted for an attachment, whose bytes still need uploading after commit.
#[derive(Debug)]
pub struct Pending {
    pub blob: Blob,
    pub data: Vec<u8>,
}

/// `record.<name>.attached?`'s blob: the attachment's blob, if any.
pub fn attached_blob(conn: &Connection, record_type: &str, record_id: i64, name: &str) -> campfire_db::Result<Option<Blob>> {
    Blob::attached(conn, record_type, record_id, name).map_err(storage_error)
}

/// Applies an assignment inside the record's save. Returns the blob to upload after commit.
pub fn assign(tx: &mut Tx<'_>, record: Record, name: &str, assignment: &Assignment) -> campfire_db::Result<Option<Pending>> {
    match assignment {
        Assignment::Unchanged => Ok(None),
        Assignment::Delete => {
            destroy(tx, record, name)?;
            Ok(None)
        }
        Assignment::Create(upload) => attach(tx, record, name, upload).map(Some),
        Assignment::Invalid => Err(campfire_db::Error::Other("Could not find or build blob: expected attachable".into())),
    }
}

/// The record an attachment belongs to: its polymorphic type and its table.
#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub record_type: &'static str,
    pub table: &'static str,
    pub id: i64,
}

impl Record {
    pub fn user(id: i64) -> Self {
        Self { record_type: "User", table: "users", id }
    }

    pub fn account(id: i64) -> Self {
        Self { record_type: "Account", table: "accounts", id }
    }
}

/// `record.<name> = uploaded_file; record.save`: replaces any current attachment.
pub fn attach(tx: &mut Tx<'_>, record: Record, name: &str, upload: &Upload) -> campfire_db::Result<Pending> {
    // Attached::Changes::CreateOne#initialize: build_after_unfurling + identify_without_saving.
    let new_blob = NewBlob::unfurl(&upload.data, Filename::new(upload.filename.clone()), upload.content_type.as_deref(), SERVICE_NAME, true);
    destroy(tx, record, name)?;
    let now = tx.now();
    let blob = new_blob.insert(tx.conn(), now.jiff()).map_err(storage_error)?;
    campfire_storage::blob::insert_attachment(tx.conn(), name, record.record_type, record.id, blob.id, now.jiff()).map_err(storage_error)?;
    super::touch(tx.conn(), record.table, record.id, tx.now())?;
    Ok(Pending { blob, data: upload.data.clone() })
}

/// `record.<name>.destroy` (the attachment): delete it, touch the record, and purge its blob
/// after commit. Nothing happens without an attachment (`delegate_missing_to :attachment, allow_nil: true`).
pub fn destroy(tx: &mut Tx<'_>, record: Record, name: &str) -> campfire_db::Result<bool> {
    let attachment: Option<(i64, i64)> = tx
        .conn()
        .query_row_cached(
            "SELECT id, blob_id FROM active_storage_attachments WHERE record_type = ?1 AND record_id = ?2 AND name = ?3 LIMIT 1",
            rusqlite::params![record.record_type, record.id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|error| if error == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(error) })?;
    let Some((attachment_id, blob_id)) = attachment else { return Ok(false) };
    tx.conn().execute_cached("DELETE FROM active_storage_attachments WHERE id = ?1", [attachment_id])?;
    super::touch(tx.conn(), record.table, record.id, tx.now())?;
    tx.emit_after_commit(Event::PurgeBlob { blob_id });
    Ok(true)
}

/// After commit: upload the file (`blob.upload_without_unfurling`), then `analyze_blob_later`.
pub async fn upload_and_analyze_later(app: &App, pending: Option<Pending>) -> Result<()> {
    let Some(Pending { blob, data }) = pending else { return Ok(()) };
    let storage = app.storage.clone();
    let key = blob.key.clone();
    let checksum = blob.checksum.clone();
    tokio::task::spawn_blocking(move || storage.service.upload(&key, data.as_slice(), checksum.as_deref()))
        .await
        .map_err(Error::internal)?
        .map_err(Error::internal)?;

    let job_app = app.clone();
    app.jobs.perform_later("ActiveStorage::AnalyzeJob", async move { analyze(&job_app, blob.id).await });
    Ok(())
}

/// `ActiveStorage::AnalyzeJob`: `blob.analyze`, then `touch_attachment_records`.
pub async fn analyze(app: &App, blob_id: i64) -> anyhow::Result<()> {
    let storage = app.storage.clone();
    app.db
        .write(move |tx| {
            let Some(mut blob) = Blob::find(tx.conn(), blob_id).map_err(storage_error)? else { return Ok(()) };
            storage.analyze(tx.conn(), &mut blob).map_err(storage_error)?;
            for (record_type, record_id) in campfire_storage::blob::attachment_records(tx.conn(), blob_id).map_err(storage_error)? {
                if let Some(table) = table_for(&record_type) {
                    super::touch(tx.conn(), table, record_id, tx.now())?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

fn table_for(record_type: &str) -> Option<&'static str> {
    match record_type {
        "User" => Some("users"),
        "Account" => Some("accounts"),
        "Message" => Some("messages"),
        _ => None,
    }
}

/// `record.<name>.variant(name).processed if record.<name>.variable?`: the processed variant's
/// blob, or `None` when there's no attachment or it can't be transformed.
pub async fn processed_variant(app: &App, record: Record, name: &str, transformations: Variation) -> Result<Option<Blob>> {
    let name = name.to_string();
    let blob = app
        .db
        .read(move |conn| attached_blob(conn, record.record_type, record.id, &name))
        .await
        .map_err(Error::internal)?;
    let Some(blob) = blob.filter(Blob::is_variable) else { return Ok(None) };
    crate::active_storage::processed_representation(app, blob, transformations).await.map(Some)
}

pub fn storage_error(error: campfire_storage::Error) -> campfire_db::Error {
    campfire_db::Error::Other(error.to_string())
}
