//! A transient file browser. These commands never consult or update the catalog.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use base64::Engine;
use serde::Serialize;

const MAX_FILES: usize = 1000;
const MAX_SCANNED: usize = 20_000;
const MAX_ERRORS: usize = 100;
static DECODERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickFile {
    path: String,
    name: String,
    size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickError {
    path: String,
    message: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickCollection {
    files: Vec<QuickFile>,
    errors: Vec<QuickError>,
    truncated: bool,
}

impl QuickCollection {
    fn error(&mut self, path: &Path, message: impl ToString) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(QuickError {
                path: path.to_string_lossy().into_owned(),
                message: message.to_string(),
            });
        }
    }
}

fn image_kind(path: &Path) -> Option<i64> {
    crate::t_utils::get_file_type(&path.to_string_lossy()).filter(|kind| matches!(kind, 1 | 3))
}

fn absolute_file(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("File paths must be absolute".into());
    }
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.is_file() {
        return Err("Not a regular file".into());
    }
    if image_kind(&path).is_none() {
        return Err("Not a supported image".into());
    }
    Ok(path)
}

fn collect_paths(paths: Vec<String>, limit: usize, scan_limit: usize) -> QuickCollection {
    let mut result = QuickCollection::default();
    let mut seen = HashSet::new();
    let mut roots = HashSet::new();
    let mut scanned = 0;
    'roots: for raw in paths {
        let input = Path::new(&raw);
        if !input.is_absolute() {
            result.error(input, "File paths must be absolute");
            continue;
        }
        let root = match input.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                result.error(input, error);
                continue;
            }
        };
        if !roots.insert(root.clone()) {
            continue;
        }
        // Do not follow directory symlinks, preventing loops and unbounded trees.
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
            if scanned >= scan_limit {
                result.truncated = true;
                break 'roots;
            }
            scanned += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.error(error.path().unwrap_or(&root), &error);
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                continue;
            }
            // Skip formats outside the image/RAW set before doing further IO.
            if image_kind(entry.path()).is_none() {
                continue;
            }
            let path = match absolute_file(entry.path()) {
                Ok(path) => path,
                Err(error) => {
                    result.error(entry.path(), error);
                    continue;
                }
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            if result.files.len() >= limit {
                result.truncated = true;
                break 'roots;
            }
            match std::fs::metadata(&path) {
                Ok(metadata) => result.files.push(QuickFile {
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    size: metadata.len(),
                }),
                Err(error) => result.error(&path, error),
            }
        }
    }
    result
}

#[tauri::command]
pub async fn quick_view_collect(paths: Vec<String>) -> Result<QuickCollection, String> {
    if paths.len() > MAX_FILES {
        return Err(format!("At most {MAX_FILES} input paths are allowed"));
    }
    tauri::async_runtime::spawn_blocking(move || collect_paths(paths, MAX_FILES, MAX_SCANNED))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickThumbnail {
    data_url: String,
}

fn thumbnail(path: &Path, size: u32) -> Result<Vec<u8>, String> {
    let path = absolute_file(path)?;
    let path = path.to_string_lossy();
    let orientation = crate::t_image::get_image_orientation(&path);
    let bytes = if image_kind(Path::new(&*path)) == Some(3) {
        crate::t_image::get_raw_thumbnail(
            &path,
            orientation,
            size,
            crate::t_raw_display::RawDisplayOptions::default(),
        )
    } else if crate::t_image::is_heic_path(&path) {
        crate::t_heif::get_heif_thumbnail(&path, orientation, size)
    } else {
        crate::t_image::get_image_thumbnail(&path, orientation, size)
    }?;
    bytes.ok_or_else(|| "No preview available for this image".into())
}

#[tauri::command]
pub async fn quick_view_thumbnail(
    path: String,
    size: Option<u32>,
) -> Result<QuickThumbnail, String> {
    let permit = DECODERS
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        let bytes = thumbnail(Path::new(&path), size.unwrap_or(384).clamp(64, 2048))?;
        let mime = if bytes.starts_with(b"\x89PNG") {
            "image/png"
        } else {
            "image/jpeg"
        };
        Ok(QuickThumbnail {
            data_url: format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn copy_paths(paths: Vec<String>) -> Result<Vec<String>, String> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(format!("Select between 1 and {MAX_FILES} images"));
    }
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for path in paths {
        // Fail atomically rather than copying only a subset of the selection.
        let canonical =
            absolute_file(Path::new(&path)).map_err(|error| format!("{path}: {error}"))?;
        if seen.insert(canonical) {
            files.push(path);
        }
    }
    Ok(files)
}

#[tauri::command]
pub async fn quick_view_copy(app: tauri::AppHandle, paths: Vec<String>) -> Result<usize, String> {
    let files = tauri::async_runtime::spawn_blocking(move || copy_paths(paths))
        .await
        .map_err(|e| e.to_string())??;
    // Offer original file URLs, without full-image decoding or a catalog lookup.
    crate::t_pasteboard::copy_files_and_image(&app, &files, None).await?;
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("lap-quick-view-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, b"fixture").unwrap();
            path.to_string_lossy().into_owned()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn collection_recurses_deduplicates_and_reports_invalid_paths() {
        let fixture = Fixture::new();
        let image = fixture.file("photo.JPG");
        fixture.file("notes.txt");
        fixture.file("movie.mp4");
        std::fs::create_dir(fixture.0.join("nested")).unwrap();
        fixture.file("nested/camera.cr2");
        let collection = collect_paths(
            vec![
                image.clone(),
                fixture.0.to_string_lossy().into_owned(),
                image,
                fixture.0.join("missing.jpg").to_string_lossy().into_owned(),
                "relative.jpg".into(),
            ],
            1000,
            100,
        );
        assert_eq!(collection.files.len(), 2);
        assert_eq!(collection.errors.len(), 2);
        assert!(!collection.truncated);
        assert!(
            collection
                .files
                .iter()
                .all(|file| Path::new(&file.path).is_absolute())
        );
    }

    #[test]
    fn collection_distinguishes_exact_limit_from_truncation_and_bounds_scan() {
        let fixture = Fixture::new();
        let images = vec![fixture.file("a.jpg"), fixture.file("b.png")];
        assert!(!collect_paths(images.clone(), 2, 100).truncated);
        let limited = collect_paths(images.clone(), 1, 100);
        assert_eq!(limited.files.len(), 1);
        assert!(limited.truncated);
        let limited = collect_paths(images, 1000, 1);
        assert_eq!(limited.files.len(), 1);
        assert!(limited.truncated);
    }

    #[test]
    fn copy_validates_entire_selection_without_silent_truncation() {
        let fixture = Fixture::new();
        let paths = (0..25)
            .map(|index| fixture.file(&format!("{index}.jpg")))
            .collect::<Vec<_>>();
        assert_eq!(copy_paths(paths.clone()).unwrap().len(), 25);
        assert_eq!(
            copy_paths(vec![paths[0].clone(), paths[0].clone()])
                .unwrap()
                .len(),
            1
        );
        assert!(copy_paths(vec![paths[0].clone(), fixture.file("not-image.txt")]).is_err());
        assert!(copy_paths(vec![paths[0].clone(); MAX_FILES + 1]).is_err());
        assert!(copy_paths(vec![]).is_err());
    }

    #[test]
    fn thumbnail_decodes_in_memory_and_rejects_corrupt_images() {
        let fixture = Fixture::new();
        let source = fixture.0.join("image.png");
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(160, 80))
            .save(&source)
            .unwrap();
        let bytes = thumbnail(&source, 64).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 32));
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
        assert!(thumbnail(Path::new(&fixture.file("corrupt.jpg")), 64).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn collection_deduplicates_file_aliases_without_following_directory_loops() {
        let fixture = Fixture::new();
        let image = fixture.file("photo.jpg");
        std::os::unix::fs::symlink(&image, fixture.0.join("alias.jpg")).unwrap();
        std::os::unix::fs::symlink(&fixture.0, fixture.0.join("loop")).unwrap();
        let collection = collect_paths(vec![fixture.0.to_string_lossy().into_owned()], 1000, 100);
        assert_eq!(collection.files.len(), 1);
        assert!(!collection.truncated);
    }
}
