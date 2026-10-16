//! File operations: details, reveal, open.
//!
//! These are Rust commands on purpose. The webview has no shell-execution permission and no
//! filesystem scope. Only a fixed File Explorer executable can be launched for a selected path;
//! scanned files are never executed by opening them with their associated application.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::filesystem::classify;
use crate::filesystem::entries::{millis, read_metadata, Kind};
use crate::platform;
use crate::state::AppState;

/// Mirrors `FileDetails`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetails {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
    pub disk_size_bytes: Option<u64>,
    pub file_count: u64,
    pub folder_count: u64,
    pub created_at: Option<i64>,
    pub modified_at: Option<i64>,
    pub accessed_at: Option<i64>,
    pub extension: Option<String>,
    pub category: String,
    pub is_symlink: bool,
    pub is_read_only: bool,
    pub parent_path: Option<String>,
}

/// Reads details for a single path. Folder totals come from the live scan index when available.
#[tauri::command]
pub fn file_details(path: String, state: State<'_, AppState>) -> AppResult<FileDetails> {
    crate::authz::authorize_state(&state, crate::authz::Permission::FileInspection)?;
    let candidate = PathBuf::from(&path);
    if path.trim().is_empty() {
        return Err(AppError::InvalidArgument(
            "No path was provided.".to_string(),
        ));
    }
    if path.ends_with("\\[Other]") {
        return Err(AppError::InvalidArgument(
            "This is an aggregate placeholder, not a real filesystem entry.".to_string(),
        ));
    }

    let meta = read_metadata(&candidate, 0).map_err(|error| AppError::io(&path, &error))?;

    // Prefer the scan's aggregated totals for folders so the numbers match the visualization.
    let (file_count, folder_count, size_bytes, disk_size_bytes) = if meta.kind == Kind::Folder {
        find_index(&state, &candidate)
            .and_then(|index| {
                index.position_of(&candidate).map(|position| {
                    let aggregate = &index.aggregates[position];
                    (
                        aggregate.file_count,
                        aggregate.folder_count,
                        aggregate.size_bytes,
                        aggregate.disk_size_bytes,
                    )
                })
            })
            .unwrap_or_default()
    } else {
        (0, 0, meta.size, meta.disk_size)
    };

    let accessed_at = std::fs::metadata(crate::filesystem::long_path::to_unc_long_path(&candidate))
        .ok()
        .and_then(|metadata| millis(metadata.accessed().ok()));

    Ok(FileDetails {
        path: path.clone(),
        name: meta.name,
        kind: if meta.kind == Kind::Folder {
            "folder"
        } else {
            "file"
        }
        .to_string(),
        size_bytes,
        disk_size_bytes,
        file_count,
        folder_count,
        created_at: meta.created_at,
        modified_at: meta.modified_at,
        accessed_at,
        extension: meta.extension,
        category: classify(&candidate, meta.kind == Kind::Folder),
        is_symlink: meta.is_symlink,
        is_read_only: {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                // FILE_ATTRIBUTE_READONLY = 0x1
                let metadata = std::fs::symlink_metadata(
                    crate::filesystem::long_path::to_unc_long_path(&candidate),
                )
                .map_err(|error| AppError::io(&path, &error))?;
                (metadata.file_attributes() & 0x1) != 0
            }
            #[cfg(not(windows))]
            {
                false
            }
        },
        parent_path: candidate
            .parent()
            .map(|parent| parent.to_string_lossy().to_string()),
    })
}

/// The most recent scan index that contains `path`, if any.
fn find_index(
    state: &State<'_, AppState>,
    path: &Path,
) -> Option<std::sync::Arc<crate::scanner::aggregate::ScanIndex>> {
    let scans = state.scans().ok()?;
    let mut best: Option<(i64, std::sync::Arc<crate::scanner::aggregate::ScanIndex>)> = None;
    for entry in scans.values() {
        if let Some(index) = entry.index.clone() {
            if index.position_of(path).is_some()
                && best
                    .as_ref()
                    .map(|(at, _)| entry.started_at > *at)
                    .unwrap_or(true)
            {
                best = Some((entry.started_at, index));
            }
        }
    }
    best.map(|(_, index)| index)
}

/// Reveals a path in File Explorer with the item selected.
#[tauri::command]
pub fn reveal_in_explorer(path: String) -> AppResult<()> {
    validate_path(&path)?;
    let candidate = PathBuf::from(&path);
    platform::reveal_in_explorer(&candidate).map_err(AppError::NotPermitted)
}

/// Opens only an ordinary folder in File Explorer; never ShellExecute a scanned file.
#[tauri::command]
pub fn open_path(path: String) -> AppResult<()> {
    validate_path(&path)?;
    let candidate = PathBuf::from(&path);
    validate_open_folder(&candidate)?;
    platform::open_folder_in_explorer(&candidate).map_err(AppError::NotPermitted)
}

fn validate_open_folder(path: &Path) -> AppResult<()> {
    if !path.is_absolute() {
        return Err(AppError::InvalidArgument(
            "An absolute folder path is required.".to_string(),
        ));
    }
    let metadata = std::fs::symlink_metadata(crate::filesystem::long_path::to_unc_long_path(path))
        .map_err(|error| AppError::io(path.to_string_lossy().to_string(), &error))?;
    #[cfg(windows)]
    let is_reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let is_reparse = metadata.file_type().is_symlink();
    if !metadata.is_dir() || is_reparse {
        return Err(AppError::NotPermitted(
            "Only ordinary folders can be opened. Reveal files in File Explorer instead."
                .to_string(),
        ));
    }
    Ok(())
}

/// Rejects anything that is not a plausible absolute filesystem path.
pub(crate) fn validate_path(path: &str) -> AppResult<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidArgument(
            "No path was provided.".to_string(),
        ));
    }
    if trimmed.contains('\0') {
        return Err(AppError::InvalidArgument(
            "The path contains a NUL byte.".to_string(),
        ));
    }
    if trimmed.ends_with("\\[Other]") {
        return Err(AppError::InvalidArgument(
            "This is an aggregate placeholder, not a real filesystem entry.".to_string(),
        ));
    }
    if !trimmed.contains(':') && !trimmed.starts_with("\\\\") {
        return Err(AppError::InvalidArgument(format!(
            "Not an absolute path: {trimmed}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_can_be_opened_not_scanned_files() {
        let folder = std::env::temp_dir().join(format!("diskwise-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("do-not-launch.exe");
        std::fs::write(&file, b"not executable").unwrap();
        assert!(validate_open_folder(&folder).is_ok());
        assert!(validate_open_folder(&file).is_err());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn rejects_empty_and_malformed_paths() {
        assert!(validate_path("").is_err());
        assert!(validate_path("   ").is_err());
        assert!(validate_path("relative\\path").is_err());
        assert!(validate_path("C:\\ok\0evil").is_err());
    }

    #[test]
    fn rejects_synthetic_other_nodes() {
        assert!(validate_path("C:\\Users\\me\\Downloads\\[Other]").is_err());
    }

    #[test]
    fn accepts_absolute_windows_paths() {
        assert!(validate_path("C:\\Users\\me").is_ok());
        assert!(validate_path("\\\\server\\share").is_ok());
        assert!(validate_path("D:\\").is_ok());
    }
}
