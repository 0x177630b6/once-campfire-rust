//! The Active Storage flows Campfire drives, over one disk service and the app verifier:
//! uploads (`create_and_upload!`), analysis, tracked variants (`VariantWithRecord`) and video
//! previews (`ActiveStorage::Preview`).
//!
//! Image and video work is blocking; call these from a blocking task.

use std::sync::Arc;

use rusqlite::Connection;
use tempfile::NamedTempFile;

use crate::analyze::Analyzer;
use crate::blob::{self, Blob, NewBlob};
use crate::disk::DiskService;
use crate::filename::Filename;
use crate::json::Json;
use crate::key::checksum_file;
use crate::marshal::Value;
use crate::process;
use crate::variation::Variation;
use crate::verifier::Verifier;
use crate::{Error, Result};

pub struct Storage {
    pub service: DiskService,
    pub verifier: Arc<dyn Verifier>,
}

impl Storage {
    pub fn new(service: DiskService, verifier: Arc<dyn Verifier>) -> Self {
        Self { service, verifier }
    }

    /// `Blob.create_and_upload!(io:, filename:, content_type:)`, as attaching an uploaded file does
    /// (`identify: true`). The row is inserted, then the bytes are uploaded with checksum
    /// verification.
    pub fn create_and_upload(
        &self,
        conn: &Connection,
        data: &[u8],
        filename: Filename,
        declared_type: Option<&str>,
        now: jiff::Timestamp,
    ) -> Result<Blob> {
        let new_blob = NewBlob::unfurl(data, filename, declared_type, self.service.name(), true);
        let checksum = new_blob.checksum.clone();
        let blob = new_blob.insert(conn, now)?;
        self.service.upload(&blob.key, data, Some(&checksum))?;
        Ok(blob)
    }

    /// `blob.open`: a tempfile named `ActiveStorage-<id>-…<.ext>`, checksum-verified.
    pub fn open(&self, blob: &Blob) -> Result<NamedTempFile> {
        let file = tempfile::Builder::new()
            .prefix(&format!("ActiveStorage-{}-", blob.id))
            .suffix(blob.filename.extension_with_delimiter())
            .tempfile()?;
        std::fs::copy(self.service.path_for(&blob.key), file.path()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { Error::FileNotFound } else { e.into() }
        })?;
        if let Some(checksum) = &blob.checksum
            && &checksum_file(file.path())? != checksum
        {
            return Err(Error::Integrity);
        }
        Ok(file)
    }

    /// `blob.analyze`: `update!(metadata: metadata.merge(analyzer.metadata.merge(analyzed: true)))`.
    /// Rails then touches the blob's attachment records (see [`blob::attachment_records`]).
    pub fn analyze(&self, conn: &Connection, blob: &mut Blob) -> Result<()> {
        let analyzer = Analyzer::for_content_type(blob.content_type());
        let mut extracted = match analyzer {
            Analyzer::Null => Json::object(),
            _ => analyzer.metadata(self.open(blob)?.path())?,
        };
        extracted.set("analyzed", Json::Bool(true));
        let mut metadata = blob.metadata.clone();
        metadata.merge(&extracted);
        blob.update_metadata(conn, metadata)
    }

    /// `blob.variant(transformations)`: the variation defaulted to the blob's variant format.
    pub fn variation_for(&self, blob: &Blob, transformations: &Variation) -> Result<Variation> {
        if !blob.is_variable() {
            return Err(Error::Invariable(blob.content_type().to_string()));
        }
        Ok(transformations.default_to(&[("format".into(), Value::Str(blob.default_variant_format()))]))
    }

    /// The processed variant's image blob, if `variant_records` already has it.
    pub fn existing_variant(&self, conn: &Connection, blob: &Blob, variation: &Variation) -> Result<Option<Blob>> {
        match blob::find_variant_record(conn, blob.id, &variation.digest())? {
            Some(record_id) => Blob::attached(conn, "ActiveStorage::VariantRecord", record_id, "image"),
            None => Ok(None),
        }
    }

    /// `VariantWithRecord#processed` for an already-defaulted variation (see [`Self::variation_for`]):
    /// reuses the variant record when present, otherwise transforms the blob and creates the
    /// variant record, its image blob and attachment, uploads it and analyzes it (Rails does the
    /// analysis in an `AnalyzeJob` after commit).
    pub fn process_variant(&self, conn: &Connection, blob: &Blob, variation: &Variation, now: jiff::Timestamp) -> Result<Blob> {
        if let Some(image) = self.existing_variant(conn, blob, variation)? {
            return Ok(image);
        }

        let output = {
            let input = self.open(blob)?;
            process::transform(input.path(), variation)?
        };
        let filename = Filename::new(format!("{}.{}", blob.filename.base(), variation.format()?.to_lowercase()));
        let content_type = variation.content_type()?;

        let Some(record_id) = blob::insert_variant_record(conn, blob.id, &variation.digest())? else {
            return self.existing_variant(conn, blob, variation)?.ok_or(Error::FileNotFound);
        };
        let mut image = NewBlob::unfurl(&output, filename, Some(&content_type), &blob.service_name, true).insert(conn, now)?;
        blob::insert_attachment(conn, "image", "ActiveStorage::VariantRecord", record_id, image.id, now)?;
        self.service.upload(&image.key, output.as_slice(), image.checksum.as_deref())?;
        self.analyze(conn, &mut image)?;
        Ok(image)
    }

    /// `blob.preview_image`, generating it with ffmpeg when missing (`Preview#process`): the
    /// frame is attached as `<base>.jpg` (`image/jpeg`) under the blob's `preview_image`.
    pub fn preview_image(&self, conn: &Connection, blob: &Blob, now: jiff::Timestamp) -> Result<Blob> {
        if let Some(image) = Blob::attached(conn, "ActiveStorage::Blob", blob.id, "preview_image")? {
            return Ok(image);
        }
        if !blob.is_previewable() {
            return Err(Error::Unpreviewable(blob.content_type().to_string()));
        }

        let frame = {
            let input = self.open(blob)?;
            process::video_preview(input.path())?
        };
        let filename = Filename::new(format!("{}.jpg", blob.filename.base()));
        let mut image = NewBlob::unfurl(&frame, filename, Some("image/jpeg"), &blob.service_name, true).insert(conn, now)?;
        blob::insert_attachment(conn, "preview_image", "ActiveStorage::Blob", blob.id, image.id, now)?;
        self.service.upload(&image.key, frame.as_slice(), image.checksum.as_deref())?;
        self.analyze(conn, &mut image)?;
        Ok(image)
    }

    /// `blob.preview(transformations).processed`, returning the blob to serve: the preview image
    /// itself for empty transformations, otherwise its processed variant.
    pub fn process_preview(&self, conn: &Connection, blob: &Blob, transformations: &Variation, now: jiff::Timestamp) -> Result<Blob> {
        let image = self.preview_image(conn, blob, now)?;
        if transformations.is_empty() {
            return Ok(image);
        }
        let variation = self.variation_for(&image, transformations)?;
        self.process_variant(conn, &image, &variation, now)
    }

    /// `blob.representation(transformations).processed`: a preview for previewable blobs, a
    /// variant for variable ones.
    pub fn process_representation(
        &self,
        conn: &Connection,
        blob: &Blob,
        transformations: &Variation,
        now: jiff::Timestamp,
    ) -> Result<Blob> {
        if blob.is_previewable() {
            self.process_preview(conn, blob, transformations, now)
        } else if blob.is_variable() {
            let variation = self.variation_for(blob, transformations)?;
            self.process_variant(conn, blob, &variation, now)
        } else {
            Err(Error::Unrepresentable(blob.content_type().to_string()))
        }
    }

    pub fn path_for(&self, blob: &Blob) -> std::path::PathBuf {
        self.service.path_for(&blob.key)
    }

    /// Deletes the blob's files (`Blob#delete`); rows are the caller's.
    pub fn delete_files(&self, blob: &Blob) -> Result<()> {
        blob.delete_files(&self.service)
    }
}
