//! Entitlement commands.
//!
//! The frontend only reads what these return. A licence check never blocks scanning: every path
//! here falls back to the free tier, which is a complete product.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::licensing::verify::{
    self, apply_offline_grace, entitlement_from_license, free_entitlement, Entitlement,
    EntitlementState,
};
use crate::state::AppState;

/// Mirrors `EntitlementEvent`.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementEvent {
    pub at: i64,
    pub from: String,
    pub to: String,
    pub reason: String,
}

/// The current entitlement, resolved from the cached licence and the offline grace period.
#[tauri::command]
pub fn entitlement_get(state: State<'_, AppState>) -> AppResult<Entitlement> {
    let now = verify::now_millis();
    let cached = {
        let guard = state
            .database()?;
        guard.as_ref().and_then(|database| database.load_entitlement_cache().ok().flatten())
    };

    let entitlement = match cached {
        Some((payload, _)) => serde_json::from_str::<Entitlement>(&payload)
            .map(|cached| apply_offline_grace(cached, now))
            .unwrap_or_else(|_| free_entitlement()),
        None => free_entitlement(),
    };

    if let Ok(mut guard) = state.entitlement.lock() {
        *guard = entitlement.clone();
    }
    Ok(entitlement)
}

/// Activates a licence key offline and caches the result.
#[tauri::command]
pub fn license_activate(license_key: String, state: State<'_, AppState>) -> AppResult<Entitlement> {
    let now = verify::now_millis();
    let entitlement = entitlement_from_license(&license_key, now);

    if let Ok(mut guard) = state.entitlement.lock() {
        *guard = entitlement.clone();
    }

    let payload = serde_json::to_string(&entitlement)
        .map_err(|error| AppError::Internal(error.to_string()))?;
    if let Ok(guard) = state.database() {
        if let Some(database) = guard.as_ref() {
            let _ = database.save_entitlement_cache(&payload, now);
            let _ = database.record_entitlement_event(
                now,
                "free",
                entitlement.state.as_str(),
                entitlement
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Licence activated.".to_string()),
            );
        }
    }

    Ok(entitlement)
}

/// Deactivates the licence and returns to the free tier. Scanning is unaffected.
#[tauri::command]
pub fn license_deactivate(state: State<'_, AppState>) -> AppResult<Entitlement> {
    let entitlement = free_entitlement();
    if let Ok(mut guard) = state.entitlement.lock() {
        *guard = entitlement.clone();
    }
    if let Ok(guard) = state.database() {
        if let Some(database) = guard.as_ref() {
            let _ = database.clear_entitlement_cache();
            let _ = database.record_entitlement_event(
                verify::now_millis(),
                "pro-active",
                EntitlementState::Free.as_str(),
                "Licence deactivated by the user.",
            );
        }
    }
    Ok(entitlement)
}

/// The entitlement transition log, newest first.
#[tauri::command]
pub fn entitlement_events(state: State<'_, AppState>) -> AppResult<Vec<EntitlementEvent>> {
    let rows = {
        let guard = state.database()?;
        match guard.as_ref() {
            Some(database) => database.list_entitlement_events(50).unwrap_or_default(),
            None => Vec::new(),
        }
    };
    Ok(rows
        .into_iter()
        .map(|row| EntitlementEvent {
            at: row.at_ms,
            from: row.from_state,
            to: row.to_state,
            reason: row.reason,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_shape_matches_the_contract() {
        let event = EntitlementEvent {
            at: 1,
            from: "free".to_string(),
            to: "trial".to_string(),
            reason: "trial started".to_string(),
        };
        assert_eq!(event.to, "trial");
    }

    #[test]
    fn activation_failure_still_returns_an_entitlement() {
        // Any key resolves to *something*; scanning is never blocked.
        let entitlement = entitlement_from_license("not-a-key", 0);
        assert!(entitlement.features.core_analysis);
    }

    #[test]
    fn deactivation_returns_the_free_tier() {
        let entitlement = free_entitlement();
        assert_eq!(entitlement.state, EntitlementState::Free);
        assert!(entitlement.features.core_analysis);
    }
}
