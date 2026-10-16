//! Cleanup execution: preview (dry run) and execute.
//!
//! The safety model lives here:
//!   - a plan is refused unless every item is explicitly listed by the caller,
//!   - every path is re-checked immediately before acting (existence, type, mtime, size),
//!   - a path that changed since the scan is skipped and reported, never acted on,
//!   - the Recycle Bin is used unless the caller explicitly asks for permanent deletion,
//!   - failures and partial completion are reported path by path,
//!   - duplicate-group deletion is not enabled by this executor until native verification can
//!     guarantee at least one byte-verified copy remains.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::filesystem::long_path::to_unc_long_path;
use crate::scanner::aggregate::{ScanIndex, OTHER_NODE_SUFFIX};

/// Mirrors `CleanupPlanItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlanItem {
    pub finding_id: String,
    /// Explicit user opt-in. Items not listed here are refused.
    pub paths: Vec<String>,
    /// `recycle` (preferred) or `permanent`.
    pub action: String,
}

/// Mirrors `CleanupPlan`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlan {
    pub scan_id: Option<String>,
    pub items: Vec<CleanupPlanItem>,
    pub dry_run: bool,
    /// One-use token returned by a native preview of these exact items.
    #[serde(default)]
    pub confirmation_token: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
}

/// Server-side proof that an exact plan was previewed recently. Never supplied by the webview.
pub struct PreviewTicket {
    pub token: String,
    pub scan_id: String,
    pub items: Vec<CleanupPlanItem>,
    pub issued_at: i64,
}

impl PreviewTicket {
    pub fn matches(&self, plan: &CleanupPlan, now: i64) -> bool {
        plan.confirmed
            && !plan.dry_run
            && plan.scan_id.as_deref() == Some(self.scan_id.as_str())
            && plan.items == self.items
            && plan.confirmation_token.as_deref() == Some(self.token.as_str())
            && (0..=120_000).contains(&(now - self.issued_at))
    }
}

/// Mirrors `CleanupPathOutcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPathOutcome {
    pub path: String,
    /// `ok`, `missing`, `denied`, `changed`, `skipped`.
    pub status: String,
    pub size_bytes: u64,
    pub detail: String,
}

/// Mirrors `CleanupPreviewItem`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPreviewItem {
    pub finding_id: String,
    pub title: String,
    pub action: String,
    pub path_count: usize,
    pub file_count: u64,
    pub folder_count: u64,
    pub size_bytes: u64,
    pub paths: Vec<CleanupPathOutcome>,
}

/// Mirrors `CleanupPreview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPreview {
    pub items: Vec<CleanupPreviewItem>,
    pub total_size_bytes: u64,
    pub total_path_count: usize,
    pub warnings: Vec<String>,
    pub confirmation_token: Option<String>,
}

/// Mirrors `CleanupResultItem`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResultItem {
    pub finding_id: String,
    pub title: String,
    pub action: String,
    pub removed_count: u64,
    pub failed_count: u64,
    pub removed_bytes: u64,
    pub results: Vec<CleanupPathOutcome>,
}

/// Mirrors `CleanupResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub items: Vec<CleanupResultItem>,
    pub total_removed_count: u64,
    pub total_failed_count: u64,
    pub total_removed_bytes: u64,
    pub completed_at: i64,
    pub partial: bool,
}

/// Everything the executor needs to revalidate a path against the scan it came from.
pub struct ValidationContext<'a> {
    pub index: &'a ScanIndex,
    /// Paths the user has marked as never-touch. Always refused.
    pub blocked_paths: &'a [String],
    /// Paths the user explicitly opted into (rule roots).
    pub approved_roots: &'a [String],
}

/// Outcome of revalidating a single path immediately before acting.
enum Check {
    Ok { size_bytes: u64 },
    Skip { reason: String },
}

fn revalidate(path: &str, context: &ValidationContext<'_>) -> Check {
    let candidate = PathBuf::from(path);
    if path.ends_with(OTHER_NODE_SUFFIX) {
        return Check::Skip {
            reason: "This is an aggregate placeholder, not a real filesystem entry.".to_string(),
        };
    }
    if candidate.as_os_str().is_empty()
        || !candidate.is_absolute()
        || candidate.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Check::Skip {
            reason: "Not an absolute, ordinary scanned file path.".to_string(),
        };
    }

    // A browser-supplied path, even under a configured root, is NEVER enough to authorize
    // deletion. It must be the exact path of a file recorded by this complete scan.
    let record = match context
        .index
        .files
        .iter()
        .find(|file| file.path == candidate)
    {
        Some(record) => record,
        None => {
            return Check::Skip {
                reason: "The file was not recorded in this scan (directories are never removed)."
                    .to_string(),
            }
        }
    };

    if !is_approved(&candidate, context.approved_roots) {
        return Check::Skip {
            reason: "This path is outside the folders you approved for cleanup.".to_string(),
        };
    }
    if is_blocked(&candidate, context.blocked_paths) {
        return Check::Skip {
            reason: "This path is on your never-touch list.".to_string(),
        };
    }
    if protected_path(&candidate) {
        return Check::Skip {
            reason:
                "Protected system, application, development or personal data cannot be cleaned up."
                    .to_string(),
        };
    }

    // Checking the parents before opening the file prevents a rule pointing through a junction
    // or a symlinked folder. Canonicalisation is not a substitute for this check: it *follows*
    // those links. A concurrent swap remains a Windows handle-level race, tracked in the audit.
    for parent in candidate.ancestors().skip(1) {
        match std::fs::symlink_metadata(to_unc_long_path(parent)) {
            Ok(metadata) if is_reparse(&metadata) => {
                return Check::Skip {
                    reason: "The path passes through a link or reparse point.".to_string(),
                }
            }
            Err(_) => {
                return Check::Skip {
                    reason: "Could not verify every parent directory.".to_string(),
                }
            }
            _ => {}
        }
    }

    let metadata = match std::fs::symlink_metadata(to_unc_long_path(&candidate)) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Check::Skip {
                reason: format!("The path could not be read ({error}); skipping."),
            }
        }
    };
    if is_reparse(&metadata) || !metadata.is_file() {
        return Check::Skip {
            reason: "Only regular scanned files may be removed, never links or directories."
                .to_string(),
        };
    }
    if metadata.permissions().readonly() {
        return Check::Skip {
            reason: "Read-only files are never removed automatically.".to_string(),
        };
    }

    // When timestamps are unavailable, we cannot establish that the file is unchanged.
    let current_mtime = crate::filesystem::entries::millis(metadata.modified().ok());
    if record.modified_at.is_none()
        || current_mtime != record.modified_at
        || metadata.len() != record.size
    {
        return Check::Skip {
            reason: "The file changed since the scan (size or modified time differs).".to_string(),
        };
    }

    Check::Ok {
        size_bytes: record.size,
    }
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

/// A second check independent of a caller-provided cleanup rule's category. Classification is
/// primarily for charts: a `cache` ancestor can override a .pdf extension, so test both.
pub(crate) fn protected_path(path: &Path) -> bool {
    const PROTECTED_PARTS: &[&str] = &[
        "windows",
        "winnt",
        "system32",
        "program files",
        "program files (x86)",
        "programdata",
        "windowsapps",
        "system volume information",
        "$recycle.bin",
        "recovery",
        "boot",
        "documents",
        "desktop",
        "onedrive",
        "user data",
        "profiles",
        "node_modules",
        ".git",
        ".cargo",
        ".rustup",
        "target",
        "__pycache__",
        ".venv",
        "vendor",
        "packages",
    ];
    if path.components().any(|part| {
        PROTECTED_PARTS.contains(&part.as_os_str().to_string_lossy().to_lowercase().as_str())
    }) {
        return true;
    }
    let category = crate::filesystem::classify(path, false);
    let extension_only = path
        .file_name()
        .map(Path::new)
        .map(|name| crate::filesystem::classify(name, false));
    [Some(category), extension_only]
        .into_iter()
        .flatten()
        .any(|kind| {
            matches!(
                kind.as_str(),
                "system" | "applications" | "documents" | "development" | "games"
            )
        })
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy()
        .to_lowercase()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string()
}

fn within(path: &Path, folder: &Path) -> bool {
    let path = normalized_path(path);
    let folder = normalized_path(folder);
    !folder.is_empty() && (path == folder || path.starts_with(&format!("{folder}\\")))
}

fn is_blocked(path: &Path, blocked: &[String]) -> bool {
    let canonical = std::fs::canonicalize(to_unc_long_path(path)).ok();
    blocked.iter().any(|entry| {
        let configured = Path::new(entry);
        within(path, configured)
            || canonical
                .as_ref()
                .zip(
                    std::fs::canonicalize(to_unc_long_path(configured))
                        .ok()
                        .as_ref(),
                )
                .is_some_and(|(actual, forbidden)| within(actual, forbidden))
    })
}

fn is_approved(path: &Path, approved: &[String]) -> bool {
    let parent = match path
        .parent()
        .and_then(|p| std::fs::canonicalize(to_unc_long_path(p)).ok())
    {
        Some(parent) => parent,
        None => return false,
    };
    approved.iter().any(|entry| {
        let root = Path::new(entry);
        let metadata = std::fs::symlink_metadata(to_unc_long_path(root)).ok();
        if !root.is_absolute() || !metadata.is_some_and(|m| m.is_dir() && !is_reparse(&m)) {
            return false;
        }
        std::fs::canonicalize(to_unc_long_path(root))
            .ok()
            .is_some_and(|root| parent.starts_with(&root))
    })
}

/// Simulates a plan without changing anything.
pub fn preview_plan(
    plan: &CleanupPlan,
    titles: &dyn Fn(&str) -> String,
    context: &ValidationContext<'_>,
) -> CleanupPreview {
    let mut items = Vec::new();
    let mut total_size_bytes = 0u64;
    let mut total_path_count = 0usize;
    let mut warnings = Vec::new();

    for item in &plan.items {
        if item.action != "recycle" && item.action != "permanent" {
            warnings.push(format!(
                "Finding {} requested an unknown action ({}); nothing was planned for it.",
                item.finding_id, item.action
            ));
            continue;
        }
        let mut outcomes = Vec::new();
        let mut size_bytes = 0u64;
        let mut file_count = 0u64;
        let folder_count = 0u64;

        for path in &item.paths {
            match revalidate(path, context) {
                Check::Ok { size_bytes: bytes } => {
                    size_bytes += bytes;
                    total_size_bytes += bytes;
                    total_path_count += 1;
                    file_count += 1; // Only ordinary scanned files can pass revalidation.
                    outcomes.push(CleanupPathOutcome {
                        path: path.clone(),
                        status: "ok".to_string(),
                        size_bytes: bytes,
                        detail: if item.action == "recycle" {
                            "Would be moved to the Recycle Bin.".to_string()
                        } else {
                            "Would be permanently deleted.".to_string()
                        },
                    });
                }
                Check::Skip { reason } => {
                    outcomes.push(CleanupPathOutcome {
                        path: path.clone(),
                        status: "skipped".to_string(),
                        size_bytes: 0,
                        detail: reason,
                    });
                }
            }
        }

        items.push(CleanupPreviewItem {
            finding_id: item.finding_id.clone(),
            title: titles(&item.finding_id),
            action: item.action.clone(),
            path_count: item.paths.len(),
            file_count,
            folder_count,
            size_bytes,
            paths: outcomes,
        });
    }

    CleanupPreview {
        items,
        total_size_bytes,
        total_path_count,
        warnings,
        confirmation_token: None, // Only the Tauri command can issue a one-use confirmation token.
    }
}

/// Executes a plan. Every path is revalidated immediately before it is touched.
pub fn execute_plan(
    plan: &CleanupPlan,
    titles: &dyn Fn(&str) -> String,
    context: &ValidationContext<'_>,
) -> CleanupResult {
    let completed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default();

    if plan.dry_run {
        return CleanupResult {
            items: Vec::new(),
            total_removed_count: 0,
            total_failed_count: 0,
            total_removed_bytes: 0,
            completed_at,
            partial: false,
        };
    }

    let mut items = Vec::new();
    let mut total_removed_count = 0u64;
    let mut total_failed_count = 0u64;
    let mut total_removed_bytes = 0u64;

    for item in &plan.items {
        if item.action != "recycle" && item.action != "permanent" {
            total_failed_count += 1;
            items.push(CleanupResultItem {
                finding_id: item.finding_id.clone(),
                title: titles(&item.finding_id),
                action: item.action.clone(),
                removed_count: 0,
                failed_count: 1,
                removed_bytes: 0,
                results: vec![CleanupPathOutcome {
                    path: String::new(),
                    status: "skipped".to_string(),
                    size_bytes: 0,
                    detail: format!("Unknown action {}.", item.action),
                }],
            });
            continue;
        }

        let mut results = Vec::new();
        let mut removed_count = 0u64;
        let mut failed_count = 0u64;
        let mut removed_bytes = 0u64;

        for path in &item.paths {
            match revalidate(path, context) {
                Check::Skip { reason } => {
                    failed_count += 1;
                    results.push(CleanupPathOutcome {
                        path: path.clone(),
                        status: "skipped".to_string(),
                        size_bytes: 0,
                        detail: reason,
                    });
                }
                Check::Ok { size_bytes } => {
                    let candidate = PathBuf::from(path);
                    match act(&candidate, &item.action) {
                        Ok(()) => {
                            removed_count += 1;
                            removed_bytes += size_bytes;
                            results.push(CleanupPathOutcome {
                                path: path.clone(),
                                status: "ok".to_string(),
                                size_bytes,
                                detail: if item.action == "recycle" {
                                    "Moved to the Recycle Bin.".to_string()
                                } else {
                                    "Permanently deleted.".to_string()
                                },
                            });
                        }
                        Err(reason) => {
                            failed_count += 1;
                            results.push(CleanupPathOutcome {
                                path: path.clone(),
                                status: "denied".to_string(),
                                size_bytes,
                                detail: reason,
                            });
                        }
                    }
                }
            }
        }

        total_removed_count += removed_count;
        total_failed_count += failed_count;
        total_removed_bytes += removed_bytes;
        items.push(CleanupResultItem {
            finding_id: item.finding_id.clone(),
            title: titles(&item.finding_id),
            action: item.action.clone(),
            removed_count,
            failed_count,
            removed_bytes,
            results,
        });
    }

    let partial = total_failed_count > 0 && total_removed_count > 0;

    CleanupResult {
        items,
        total_removed_count,
        total_failed_count,
        total_removed_bytes,
        completed_at,
        partial,
    }
}

/// The single place where anything is actually removed.
fn act(path: &Path, action: &str) -> Result<(), String> {
    let long = to_unc_long_path(path);
    if action == "permanent" {
        // Do not ever recurse: a directory may have acquired new contents since the scan.
        std::fs::remove_file(&long).map_err(|error| error.to_string())
    } else {
        // Preferred path: the Recycle Bin, so the action is reversible.
        trash::delete(&long).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::aggregate::ScanIndex;
    use crate::scanner::walker::ScanOptions;
    use std::fs;
    use std::sync::Arc;

    fn temp_dir(name: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("diskwise-exec-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn scan_index(root: &Path) -> ScanIndex {
        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            ..Default::default()
        };
        let counters = Arc::new(crate::scanner::progress::ProgressCounters::new());
        let run = crate::scanner::walker::walk(&options, &counters);
        ScanIndex::from_run(&run)
    }

    fn titles(_id: &str) -> String {
        "Test finding".to_string()
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let root = temp_dir("dry");
        fs::create_dir_all(root.join("cache")).unwrap();
        let victim = root.join("cache\\junk.bin");
        fs::write(&victim, vec![0u8; 128]).unwrap();
        let index = scan_index(&root);
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![victim.to_string_lossy().to_string()],
                action: "recycle".to_string(),
            }],
            dry_run: true,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let preview = preview_plan(&plan, &titles, &context);
        assert_eq!(preview.total_path_count, 1);
        assert_eq!(preview.total_size_bytes, 128);
        assert!(victim.exists(), "a dry run must not touch the filesystem");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_outside_the_approved_roots_is_refused() {
        let root = temp_dir("refuse");
        fs::create_dir_all(root.join("docs")).unwrap();
        let victim = root.join("docs\\report.pdf");
        fs::write(&victim, vec![0u8; 64]).unwrap();
        let index = scan_index(&root);
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![victim.to_string_lossy().to_string()],
                action: "recycle".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert_eq!(result.total_failed_count, 1);
        assert!(victim.exists(), "the file must survive");
        assert!(result.items[0].results[0]
            .detail
            .contains("outside the folders you approved"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_blocked_path_is_refused_even_when_approved() {
        let root = temp_dir("blocked");
        fs::create_dir_all(root.join("cache")).unwrap();
        let victim = root.join("cache\\keep.bin");
        fs::write(&victim, vec![0u8; 32]).unwrap();
        let index = scan_index(&root);
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![victim.to_string_lossy().to_string()],
                action: "permanent".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[victim.to_string_lossy().to_string()],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(victim.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_changed_since_the_scan_is_skipped() {
        let root = temp_dir("changed");
        fs::create_dir_all(root.join("cache")).unwrap();
        let victim = root.join("cache\\grows.bin");
        fs::write(&victim, vec![0u8; 100]).unwrap();
        let index = scan_index(&root);
        // The file changes after the scan.
        fs::write(&victim, vec![0u8; 250]).unwrap();

        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![victim.to_string_lossy().to_string()],
                action: "permanent".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert_eq!(result.total_failed_count, 1);
        assert!(result.items[0].results[0]
            .detail
            .contains("changed since the scan"));
        assert!(victim.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_path_is_reported_as_skipped() {
        let root = temp_dir("missing");
        fs::create_dir_all(root.join("cache")).unwrap();
        let index = scan_index(&root);
        let ghost = root.join("cache\\ghost.bin");
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![ghost.to_string_lossy().to_string()],
                action: "recycle".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert_eq!(result.items[0].results[0].status, "skipped");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_action_is_refused_with_a_warning() {
        let root = temp_dir("action");
        fs::create_dir_all(root.join("cache")).unwrap();
        let index = scan_index(&root);
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: Vec::new(),
                action: "shred".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[],
        };
        let preview = preview_plan(&plan, &titles, &context);
        assert!(!preview.warnings.is_empty());
        assert_eq!(preview.total_path_count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_synthetic_other_node_is_never_actionable() {
        let root = temp_dir("synthetic");
        fs::create_dir_all(root.join("cache")).unwrap();
        let index = scan_index(&root);
        let synthetic = format!("{}\\cache\\[Other]", root.to_string_lossy());
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![synthetic],
                action: "permanent".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(result.items[0].results[0]
            .detail
            .contains("aggregate placeholder"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn partial_completion_is_reported() {
        let root = temp_dir("partial");
        fs::create_dir_all(root.join("cache")).unwrap();
        let real = root.join("cache\\real.bin");
        fs::write(&real, vec![0u8; 16]).unwrap();
        let index = scan_index(&root);
        let plan = CleanupPlan {
            scan_id: None,
            items: vec![CleanupPlanItem {
                finding_id: "f1".to_string(),
                paths: vec![
                    real.to_string_lossy().to_string(),
                    root.join("cache\\ghost.bin").to_string_lossy().to_string(),
                ],
                action: "permanent".to_string(),
            }],
            dry_run: false,
            confirmation_token: None,
            confirmed: false,
        };
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[root.join("cache").to_string_lossy().to_string()],
        };
        let result = execute_plan(&plan, &titles, &context);
        assert_eq!(result.total_removed_count, 1);
        assert_eq!(result.total_failed_count, 1);
        assert!(result.partial);

        let _ = fs::remove_dir_all(&root);
    }
    fn one_file_plan(path: &Path, dry_run: bool) -> CleanupPlan {
        CleanupPlan {
            scan_id: Some("scan-1".to_string()),
            items: vec![CleanupPlanItem {
                finding_id: "rule-1".to_string(),
                paths: vec![path.to_string_lossy().to_string()],
                action: "permanent".to_string(),
            }],
            dry_run,
            confirmation_token: None,
            confirmed: false,
        }
    }

    #[test]
    fn execute_plan_cannot_override_a_dry_run_to_mutate() {
        let root = temp_dir("dry-run-execute");
        let approved = root.join("cache");
        fs::create_dir(&approved).unwrap();
        let victim = approved.join("junk.bin");
        fs::write(&victim, [1u8; 8]).unwrap();
        let index = scan_index(&root);
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[approved.to_string_lossy().to_string()],
        };
        let result = execute_plan(&one_file_plan(&victim, true), &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(victim.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_files_and_directories_are_not_actionable_after_a_scan() {
        let root = temp_dir("new-content");
        let approved = root.join("cache");
        fs::create_dir(&approved).unwrap();
        let index = scan_index(&root);
        let new_file = approved.join("new.bin");
        fs::write(&new_file, [2u8; 8]).unwrap();
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[approved.to_string_lossy().to_string()],
        };
        for target in [&approved, &new_file] {
            let result = execute_plan(&one_file_plan(target, false), &titles, &context);
            assert_eq!(result.total_removed_count, 0);
            assert_eq!(result.total_failed_count, 1);
        }
        assert!(new_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_size_but_new_mtime_is_not_unchanged() {
        let root = temp_dir("mtime-change");
        let approved = root.join("cache");
        fs::create_dir(&approved).unwrap();
        let victim = approved.join("blob.bin");
        fs::write(&victim, [1u8; 8]).unwrap();
        let index = scan_index(&root);
        let file = fs::OpenOptions::new().write(true).open(&victim).unwrap();
        let later =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
        file.set_times(fs::FileTimes::new().set_modified(later))
            .unwrap();
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[approved.to_string_lossy().to_string()],
        };
        let result = execute_plan(&one_file_plan(&victim, false), &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(victim.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_protected_document_under_an_approved_cache_root_survives() {
        let root = temp_dir("protected-file");
        let approved = root.join("cache");
        fs::create_dir(&approved).unwrap();
        let victim = approved.join("report.pdf");
        fs::write(&victim, [1u8; 8]).unwrap();
        let index = scan_index(&root);
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[approved.to_string_lossy().to_string()],
        };
        let result = execute_plan(&one_file_plan(&victim, false), &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(victim.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_replaced_by_a_symlink_is_refused() {
        let root = temp_dir("parent-swap");
        let outside = temp_dir("parent-swap-target");
        let approved = root.join("cache");
        fs::create_dir(&approved).unwrap();
        let victim = approved.join("blob.bin");
        fs::write(&victim, [1u8; 8]).unwrap();
        let index = scan_index(&root);
        fs::remove_dir_all(&approved).unwrap();
        fs::write(outside.join("blob.bin"), [1u8; 8]).unwrap();
        std::os::unix::fs::symlink(&outside, &approved).unwrap();
        let context = ValidationContext {
            index: &index,
            blocked_paths: &[],
            approved_roots: &[approved.to_string_lossy().to_string()],
        };
        let result = execute_plan(&one_file_plan(&victim, false), &titles, &context);
        assert_eq!(result.total_removed_count, 0);
        assert!(outside.join("blob.bin").exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn preview_tickets_are_bound_to_exact_actions_and_expire() {
        let mut plan = one_file_plan(Path::new("C:\\Temp\\a.bin"), false);
        plan.items[0].action = "recycle".to_string();
        plan.confirmed = true;
        plan.confirmation_token = Some("nonce".to_string());
        let ticket = PreviewTicket {
            token: "nonce".to_string(),
            scan_id: "scan-1".to_string(),
            items: plan.items.clone(),
            issued_at: 1000,
        };
        assert!(ticket.matches(&plan, 1001));
        assert!(!ticket.matches(&plan, 121_001));
        plan.items[0].paths.push("C:\\Temp\\new.bin".to_string());
        assert!(!ticket.matches(&plan, 1001));
        plan.items = ticket.items.clone();
        plan.dry_run = true;
        assert!(!ticket.matches(&plan, 1001));
    }
}
