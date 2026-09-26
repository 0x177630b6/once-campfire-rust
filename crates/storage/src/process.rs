//! Producing variant and preview bytes: `ActiveStorage::Transformers::Vips` (image_processing
//! 1.14) and `ActiveStorage::Previewer::VideoPreviewer`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::content_types::{VIDEO_PREVIEW_ARGUMENTS, ffmpeg_path};
use crate::marshal::Value;
use crate::variation::Variation;
use crate::vips::Image;
use crate::{Error, Result};

/// `variation.transform(file)`: `ImageProcessing::Vips.source(file).loader(page: 0)
/// .convert(format).apply(operations).call`, saved to a tempfile named for the format.
pub fn transform(input: &Path, variation: &Variation) -> Result<Vec<u8>> {
    let format = variation.format()?;
    let operations = operations(variation)?;

    let mut image = Image::load_for_processing(input)?;
    for (width, height) in operations {
        image = image.resize_to_limit(width, height)?;
    }

    let output = tempfile::Builder::new().prefix("image_processing").suffix(&format!(".{format}")).tempfile()?;
    image.write_to_file(output.path())?;
    Ok(std::fs::read(output.path())?)
}

/// `ImageProcessingTransformer#operations`: every transformation except `format`, skipping blank
/// arguments. Only `resize_to_limit` is implemented: it's the only one Campfire defines.
fn operations(variation: &Variation) -> Result<Vec<(Option<i32>, Option<i32>)>> {
    let mut operations = Vec::new();
    for (name, argument) in variation.transformations() {
        match (name.as_str(), argument) {
            ("format", _) => {}
            ("combine_options", _) => {
                return Err(Error::InvalidVariation("combine_options is not supported".into()));
            }
            (_, argument) if blank(argument) => {}
            ("resize_to_limit", Value::Array(args)) if args.len() == 2 => {
                let dimension = |v: Option<&Value>| match v {
                    None | Some(Value::Nil) => Ok(None),
                    Some(Value::Int(n)) => Ok(Some(*n as i32)),
                    Some(other) => Err(Error::InvalidVariation(format!("resize_to_limit argument {other:?}"))),
                };
                let (width, height) = (dimension(args.first())?, dimension(args.get(1))?);
                if width.is_none() && height.is_none() {
                    return Err(Error::InvalidVariation("either width or height must be specified".into()));
                }
                operations.push((width, height));
            }
            (name, argument) => {
                return Err(Error::InvalidVariation(format!("unsupported transformation {name}: {argument:?}")));
            }
        }
    }
    Ok(operations)
}

/// `Object#present?` negated, for the values a transformation can hold.
fn blank(value: &Value) -> bool {
    match value {
        Value::Nil | Value::Bool(false) => true,
        Value::Str(s) => s.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Hash(entries) => entries.is_empty(),
        _ => false,
    }
}

/// `VideoPreviewer.accept?`: `system(ffmpeg, "-version")`, memoized.
pub fn ffmpeg_exists() -> bool {
    static EXISTS: OnceLock<bool> = OnceLock::new();
    *EXISTS.get_or_init(|| {
        Command::new(ffmpeg_path())
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// `draw_relevant_frame_from`: `ffmpeg -i <input> <video_preview_arguments> -`, capturing stdout.
pub fn video_preview(input: &Path) -> Result<Vec<u8>> {
    let output = Command::new(ffmpeg_path())
        .arg("-i")
        .arg(input)
        .args(VIDEO_PREVIEW_ARGUMENTS)
        .arg("-")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::Preview(format!(
            "{} failed (status {}): {}",
            ffmpeg_path(),
            output.status.code().map_or("nil".into(), |c| c.to_string()),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )));
    }
    Ok(output.stdout)
}
