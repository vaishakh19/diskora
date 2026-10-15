//! Cleanup commands.
//!
//! `cleanup_execute` refuses to run unless the plan is explicit: every item must list the exact
//! paths the user chose, and each one is revalidated immediately before it is touched.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tauri::State;

use crate::cleanup::execute::{
    self, CleanupPlan, CleanupPreview, CleanupResult, PreviewTicket, ValidationContext,
};
use crate::cleanup::rules::{self, CleanupFinding, CleanupRule, RecycleBinSummary};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Mirrors `CleanupFinding` (returned as-is).
pub use crate::cleanup::rules::CleanupFinding as Finding;

/// Builds findings for a scan from the user's rules. Free at every tier.
#[tauri::command]
pub fn cleanup_scan(
    scan_id: String,
    rules: Vec<CleanupRule>,
    state: State<'_, AppState>,
) -> AppResult<Vec<CleanupFinding>> {
    crate::authz::authorize_state(&state, crate::authz::Permission::CoreAnalysis)?;
    let scans = state.scans()?;
    let entry = scans
        .get(&scan_id)
        .ok_or_else(|| AppError::NotFound(format!("Unknown scan {scan_id}")))?;
    if entry.status != "complete" || entry.hit_limit || !entry.errors.is_empty() {
        return Err(AppError::NotPermitted(
            "Cleanup requires a complete scan without unreadable or skipped locations.".to_string(),
        ));
    }
    let index = entry
        .index
        .clone()
        .ok_or_else(|| AppError::NotFound("The scan is still running.".to_string()))?;
    drop(scans);

    if rules.len() > 32
        || rules
            .iter()
            .any(|rule| rule.id.trim().is_empty() || !Path::new(&rule.path).is_absolute())
    {
        return Err(AppError::InvalidArgument(
            "Provide at most 32 rules with an ID and an absolute folder path.".to_string(),
        ));
    }
    let unique: HashSet<&str> = rules.iter().map(|rule| rule.id.as_str()).collect();
    if unique.len() != rules.len() {
        return Err(AppError::InvalidArgument(
            "Cleanup rule IDs must be unique.".to_string(),
        ));
    }
    let now = crate::licensing::verify::now_millis();
    let findings = rules::build_findings(&index, &rules, now);
    *state
        .cleanup_findings
        .lock()
        .map_err(|_| AppError::Internal("The cleanup findings lock was poisoned.".to_string()))? =
        Some((scan_id, findings.clone()));
    *state
        .cleanup_preview
        .lock()
        .map_err(|_| AppError::Internal("The cleanup preview lock was poisoned.".to_string()))? =
        None;
    Ok(findings)
}

/// Simulates a plan. Changes nothing, and is free at every tier.
#[tauri::command]
pub fn cleanup_preview(plan: CleanupPlan, state: State<'_, AppState>) -> AppResult<CleanupPreview> {
    crate::authz::authorize_state(&state, crate::authz::Permission::PreviewOnly)?;
    let prepared = prepare(&plan, &state)?;
    let context = ValidationContext {
        index: &prepared.index,
        blocked_paths: &prepared.blocked,
        approved_roots: &prepared.approved,
    };
    let title = |id: &str| prepared.titles.get(id).cloned().unwrap_or_default();
    let mut preview = execute::preview_plan(&plan, &title, &context);
    let safe = preview.total_path_count > 0
        && preview.warnings.is_empty()
        && preview
            .items
            .iter()
            .all(|item| item.paths.iter().all(|path| path.status == "ok"))
        && plan.items.iter().all(|item| item.action == "recycle");
    let mut registry = state
        .cleanup_preview
        .lock()
        .map_err(|_| AppError::Internal("The cleanup preview lock was poisoned.".to_string()))?;
    *registry = None; // Invalidate any older preview, even when this request was unsafe.
    if safe {
        let token = uuid::Uuid::new_v4().to_string();
        *registry = Some(PreviewTicket {
            token: token.clone(),
            scan_id: plan.scan_id.clone().unwrap_or_default(),
            items: plan.items.clone(),
            issued_at: crate::licensing::verify::now_millis(),
        });
        preview.confirmation_token = Some(token);
    } else {
        preview.warnings.push(
            "Nothing can be executed until all paths pass validation. Permanent deletion is disabled in this build."
                .to_string(),
        );
    }
    Ok(preview)
}

/// Executes a plan. Every path is revalidated immediately before it is touched.
///
/// This is the single most dangerous command. It requires a native paid entitlement and a
/// one-use, confirmed preview of the exact files and recycle actions. Permanent deletion is
/// disabled until NTFS handle-level safety can be verified. UI changes cannot bypass these checks.
#[tauri::command]
pub fn cleanup_execute(plan: CleanupPlan, state: State<'_, AppState>) -> AppResult<CleanupResult> {
    if plan.dry_run {
        return Err(AppError::InvalidArgument(
            "Dry runs must use cleanup_preview; cleanup_execute never acts on a dry-run plan."
                .to_string(),
        ));
    }
    crate::authz::authorize_state(&state, crate::authz::Permission::PaidCleanup)?;

    // Permanent deletion cannot be made safe enough with path-based removal on an unverified NTFS
    // runner. Do not expose it until handle-based revalidation and native tests are in place.
    if plan.items.iter().any(|item| item.action == "permanent") {
        return Err(AppError::NotPermitted(
            "Permanent deletion is disabled pending Windows safety verification.".to_string(),
        ));
    }

    // A plan that touches many paths at once is a bulk operation, which is its own capability.
    let total_paths: usize = plan.items.iter().map(|item| item.paths.len()).sum();
    if total_paths > 50 {
        crate::authz::authorize_state(&state, crate::authz::Permission::BulkFileOperations)?;
    }

    let ticket = state
        .cleanup_preview
        .lock()
        .map_err(|_| AppError::Internal("The cleanup preview lock was poisoned.".to_string()))?
        .take(); // A preview is single-use, including failed attempts.
    if !ticket.is_some_and(|ticket| ticket.matches(&plan, crate::licensing::verify::now_millis())) {
        return Err(AppError::NotPermitted(
            "Preview these exact files, then explicitly confirm within two minutes.".to_string(),
        ));
    }
    let prepared = prepare(&plan, &state)?;
    let context = ValidationContext {
        index: &prepared.index,
        blocked_paths: &prepared.blocked,
        approved_roots: &prepared.approved,
    };
    // The executor revalidates every path immediately before acting, so the result below is the
    // authoritative account of what happened — including anything that was skipped.
    let title = |id: &str| prepared.titles.get(id).cloned().unwrap_or_default();
    Ok(execute::execute_plan(&plan, &title, &context))
}

/// Recycle Bin contents, via the Windows shell.
#[tauri::command]
pub fn recycle_bin_summary() -> RecycleBinSummary {
    let info = crate::platform::recycle_bin(None);
    RecycleBinSummary {
        item_count: info.item_count,
        size_bytes: if info.supported {
            Some(info.size_bytes)
        } else {
            None
        },
        supported: info.supported,
        detail: info.detail,
    }
}

/// Everything the executor needs for a plan.
struct PreparedPlan {
    index: std::sync::Arc<crate::scanner::aggregate::ScanIndex>,
    approved: Vec<String>,
    blocked: Vec<String>,
    titles: HashMap<String, String>,
}

/// Resolves everything the executor needs for a plan.
fn prepare(plan: &CleanupPlan, state: &State<'_, AppState>) -> AppResult<PreparedPlan> {
    let scan_id = plan
        .scan_id
        .clone()
        .ok_or_else(|| AppError::InvalidArgument("A plan must name its scan.".to_string()))?;

    let scans = state.scans()?;
    let entry = scans
        .get(&scan_id)
        .ok_or_else(|| AppError::NotFound(format!("Unknown scan {scan_id}")))?
        .clone();
    drop(scans);

    if entry.status != "complete" || entry.hit_limit || !entry.errors.is_empty() {
        return Err(AppError::NotPermitted(
            "Cleanup requires a complete scan without unreadable or skipped locations.".to_string(),
        ));
    }
    let index = entry
        .index
        .clone()
        .ok_or_else(|| AppError::NotFound("The scan is still running.".to_string()))?;

    let findings = state
        .cleanup_findings
        .lock()
        .map_err(|_| AppError::Internal("The cleanup findings lock was poisoned.".to_string()))?;
    let verified = match findings.as_ref() {
        Some((stored_id, findings)) if *stored_id == scan_id => findings,
        _ => {
            return Err(AppError::NotPermitted(
                "Run cleanup analysis for this scan before previewing files.".to_string(),
            ))
        }
    };
    let (approved, titles) = validate_plan_items(plan, verified)?;
    drop(findings);

    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Internal("The settings lock was poisoned.".to_string()))?
        .clone()
        .unwrap_or_default();
    Ok(PreparedPlan {
        index,
        approved,
        blocked: settings.exclude_paths,
        titles,
    })
}

/// Only server-generated, non-protected findings authorize exact scanned file paths.
fn validate_plan_items(
    plan: &CleanupPlan,
    verified: &[CleanupFinding],
) -> AppResult<(Vec<String>, HashMap<String, String>)> {
    if plan.items.is_empty() || plan.items.len() > 32 {
        return Err(AppError::InvalidArgument(
            "Select between 1 and 32 findings.".to_string(),
        ));
    }
    let total_paths: usize = plan.items.iter().map(|item| item.paths.len()).sum();
    if total_paths == 0 || total_paths > 100 {
        return Err(AppError::InvalidArgument(
            "Select between 1 and 100 individual files per cleanup.".to_string(),
        ));
    }

    let mut titles = HashMap::new();
    let mut approved = Vec::new();
    let mut seen = HashSet::new();
    for item in &plan.items {
        let finding = verified
            .iter()
            .find(|finding| finding.id == item.finding_id)
            .ok_or_else(|| AppError::NotPermitted("Unknown cleanup finding.".to_string()))?;
        if finding.protected || finding.risk == "informational" || finding.path.is_none() {
            return Err(AppError::NotPermitted(
                "This finding is informational only.".to_string(),
            ));
        }
        if item.action != "recycle" && item.action != "permanent" {
            return Err(AppError::InvalidArgument(
                "Unknown cleanup action.".to_string(),
            ));
        }
        for path in &item.paths {
            if !seen.insert(path) || finding.paths.binary_search(path).is_err() {
                return Err(AppError::NotPermitted(
                    "Every selected file must belong to its original eligible finding, only once."
                        .to_string(),
                ));
            }
        }
        titles.insert(finding.id.clone(), finding.title.clone());
        approved.push(finding.path.clone().unwrap_or_default());
    }
    Ok((approved, titles))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (CleanupFinding, CleanupPlan) {
        let file = "C:\\Temp\\junk.bin".to_string();
        let finding = CleanupFinding {
            id: "rule-1".to_string(),
            category: "temp".to_string(),
            risk: "review".to_string(),
            title: "Temp".to_string(),
            reason: "User selected temp".to_string(),
            protected: false,
            path: Some("C:\\Temp".to_string()),
            paths: vec![file.clone()],
            file_count: 1,
            folder_count: 0,
            size_bytes: 5,
            disk_size_bytes: None,
            oldest_modified_at: None,
            newest_modified_at: None,
            rule_id: Some("1".to_string()),
        };
        let plan = CleanupPlan {
            scan_id: Some("scan-1".to_string()),
            items: vec![crate::cleanup::execute::CleanupPlanItem {
                finding_id: finding.id.clone(),
                paths: vec![file],
                action: "recycle".to_string(),
            }],
            dry_run: true,
            confirmation_token: None,
            confirmed: false,
        };
        (finding, plan)
    }

    #[test]
    fn plan_cannot_invent_finding_ids_or_files_even_under_an_approved_root() {
        let (finding, mut plan) = sample();
        assert!(validate_plan_items(&plan, &[finding.clone()]).is_ok());
        plan.items[0].finding_id = "forged".to_string();
        assert!(validate_plan_items(&plan, &[finding.clone()]).is_err());
        plan.items[0].finding_id = finding.id.clone();
        plan.items[0].paths = vec!["C:\\Temp\\new.bin".to_string()];
        assert!(validate_plan_items(&plan, &[finding.clone()]).is_err());
        plan.items[0].paths = vec![finding.paths[0].clone(); 2];
        assert!(validate_plan_items(&plan, &[finding]).is_err());
    }

    #[test]
    fn informational_findings_can_never_authorize_a_plan() {
        let (mut finding, plan) = sample();
        finding.protected = true;
        assert!(validate_plan_items(&plan, &[finding.clone()]).is_err());
        finding.protected = false;
        finding.risk = "informational".to_string();
        assert!(validate_plan_items(&plan, &[finding]).is_err());
    }
}
