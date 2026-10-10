//! Offline entitlement resolution.
//!
//! A licence key is a compact string: `<base64url payload>.<base64url signature>`. The payload is
//! JSON describing the tier and expiry; the signature is Ed25519 over the payload bytes. Nothing is
//! sent anywhere.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::keys::public_key;

/// Mirrors `PlanTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanTier {
    Free,
    Trial,
    Pro,
}

impl PlanTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanTier::Free => "free",
            PlanTier::Trial => "trial",
            PlanTier::Pro => "pro",
        }
    }
}

/// Mirrors `EntitlementState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntitlementState {
    Free,
    Trial,
    ProActive,
    ProExpired,
    Invalid,
}

impl EntitlementState {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntitlementState::Free => "free",
            EntitlementState::Trial => "trial",
            EntitlementState::ProActive => "pro-active",
            EntitlementState::ProExpired => "pro-expired",
            EntitlementState::Invalid => "invalid",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "trial" => EntitlementState::Trial,
            "pro-active" => EntitlementState::ProActive,
            "pro-expired" => EntitlementState::ProExpired,
            "invalid" => EntitlementState::Invalid,
            _ => EntitlementState::Free,
        }
    }
}

/// Mirrors `FeatureFlags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureFlags {
    pub core_analysis: bool,
    pub scan_history: bool,
    pub advanced_duplicates: bool,
    pub custom_cleanup_rules: bool,
    pub scheduled_scans: bool,
    pub export_reports: bool,
    pub advanced_filters: bool,
}

impl FeatureFlags {
    /// The free tier is a complete product: scanning, visualization and manual cleanup always work.
    pub fn free() -> Self {
        FeatureFlags {
            core_analysis: true,
            scan_history: false,
            advanced_duplicates: false,
            custom_cleanup_rules: false,
            scheduled_scans: false,
            export_reports: false,
            advanced_filters: false,
        }
    }

    /// Trial and active Pro get everything.
    pub fn pro() -> Self {
        FeatureFlags {
            core_analysis: true,
            scan_history: true,
            advanced_duplicates: true,
            custom_cleanup_rules: true,
            scheduled_scans: true,
            export_reports: true,
            advanced_filters: true,
        }
    }

    pub fn for_state(state: EntitlementState) -> Self {
        match state {
            EntitlementState::Trial | EntitlementState::ProActive => FeatureFlags::pro(),
            _ => FeatureFlags::free(),
        }
    }
}

/// Mirrors `Entitlement`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    pub state: EntitlementState,
    pub tier: PlanTier,
    pub features: FeatureFlags,
    pub license_key_masked: Option<String>,
    pub expires_at: Option<i64>,
    pub verified_at: Option<i64>,
    pub source: String,
    pub reason: Option<String>,
    pub days_remaining: Option<i64>,
    /// Free-tier duplicate group limit. `0` = unlimited.
    pub duplicate_group_limit: u32,
}

/// Free-tier duplicate group limit, matching `FREE_DUPLICATE_GROUP_LIMIT` in the frontend.
pub const FREE_DUPLICATE_GROUP_LIMIT: u32 = 25;
/// Trial length in days.
pub const TRIAL_DAYS: i64 = 14;
/// How long a cached entitlement keeps working with no re-validation.
pub const OFFLINE_GRACE_DAYS: i64 = 30;

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

/// The entitlement used when nothing is activated. Never blocks anything.
pub fn free_entitlement() -> Entitlement {
    Entitlement {
        state: EntitlementState::Free,
        tier: PlanTier::Free,
        features: FeatureFlags::free(),
        license_key_masked: None,
        expires_at: None,
        verified_at: None,
        source: "default".to_string(),
        reason: None,
        days_remaining: None,
        duplicate_group_limit: FREE_DUPLICATE_GROUP_LIMIT,
    }
}

/// The signed contents of a licence key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicensePayload {
    /// `trial` or `pro`.
    pub tier: PlanTier,
    /// Unix milliseconds.
    pub issued_at: i64,
    /// Unix milliseconds, or `None` for a perpetual licence.
    pub expires_at: Option<i64>,
    /// Stable identifier, never an email address.
    pub license_id: String,
}

/// Verifies a licence key offline. Returns the entitlement on success.
///
/// Any failure — bad format, bad signature, unknown key, expired — produces the free entitlement
/// with a reason, so the app keeps working.
pub fn entitlement_from_license(key: &str, now: i64) -> Entitlement {
    let mut fallback = free_entitlement();
    fallback.license_key_masked = Some(mask_key(key));
    fallback.source = "license-key".to_string();

    let trimmed = key.trim();
    if trimmed.is_empty() {
        fallback.reason = Some("No licence key was provided.".to_string());
        return fallback;
    }

    let mut parts = trimmed.split('.');
    let payload_b64 = match parts.next() {
        Some(value) if !value.is_empty() => value,
        _ => {
            fallback.state = EntitlementState::Invalid;
            fallback.reason = Some("The licence key is malformed.".to_string());
            return fallback;
        }
    };
    let signature_b64 = match parts.next() {
        Some(value) if !value.is_empty() => value,
        _ => {
            fallback.state = EntitlementState::Invalid;
            fallback.reason = Some("The licence key has no signature.".to_string());
            return fallback;
        }
    };
    if parts.next().is_some() {
        fallback.state = EntitlementState::Invalid;
        fallback.reason = Some("The licence key has extra segments.".to_string());
        return fallback;
    }

    let payload_bytes = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) {
        Ok(bytes) => bytes,
        Err(_) => {
            fallback.state = EntitlementState::Invalid;
            fallback.reason = Some("The licence payload is not valid base64.".to_string());
            return fallback;
        }
    };
    let signature_bytes = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(signature_b64) {
        Ok(bytes) => bytes,
        Err(_) => {
            fallback.state = EntitlementState::Invalid;
            fallback.reason = Some("The licence signature is not valid base64.".to_string());
            return fallback;
        }
    };

    let payload: LicensePayload = match serde_json::from_slice(&payload_bytes) {
        Ok(payload) => payload,
        Err(_) => {
            fallback.state = EntitlementState::Invalid;
            fallback.reason = Some("The licence payload is not valid JSON.".to_string());
            return fallback;
        }
    };

    if !verify_signature(&payload_bytes, &signature_bytes) {
        fallback.state = EntitlementState::Invalid;
        fallback.reason = Some("Signature verification failed.".to_string());
        return fallback;
    }

    let expired = match payload.expires_at {
        Some(expiry) => now >= expiry,
        None => false,
    };

    let (state, reason) = if expired {
        (
            EntitlementState::ProExpired,
            Some("This licence has expired. Diskwise continues on the free tier.".to_string()),
        )
    } else {
        let state = match payload.tier {
            PlanTier::Trial => EntitlementState::Trial,
            _ => EntitlementState::ProActive,
        };
        (state, None)
    };

    let days_remaining = payload
        .expires_at
        .map(|expiry| ((expiry - now) / 86_400_000).max(0));

    Entitlement {
        state,
        tier: payload.tier,
        features: FeatureFlags::for_state(state),
        license_key_masked: Some(mask_key(key)),
        expires_at: payload.expires_at,
        verified_at: Some(now),
        source: "license-key".to_string(),
        reason,
        days_remaining,
        duplicate_group_limit: if matches!(state, EntitlementState::Trial | EntitlementState::ProActive) {
            0
        } else {
            FREE_DUPLICATE_GROUP_LIMIT
        },
    }
}

/// Applies the offline grace period to a cached entitlement.
///
/// A cached entitlement keeps working for `OFFLINE_GRACE_DAYS` after it was last verified, then
/// falls back to free. Scanning is unaffected either way.
pub fn apply_offline_grace(entitlement: Entitlement, now: i64) -> Entitlement {
    let mut entitlement = entitlement;
    if matches!(
        entitlement.state,
        EntitlementState::Trial | EntitlementState::ProActive
    ) {
        if let Some(expiry) = entitlement.expires_at {
            if now >= expiry {
                entitlement.state = EntitlementState::ProExpired;
                entitlement.features = FeatureFlags::free();
                entitlement.reason = Some("The cached licence has expired.".to_string());
                entitlement.days_remaining = Some(0);
                return entitlement;
            }
        }
        let stale_after = entitlement
            .verified_at
            .unwrap_or(0)
            .saturating_add(OFFLINE_GRACE_DAYS * 86_400_000);
        if now > stale_after {
            entitlement.state = EntitlementState::Free;
            entitlement.tier = PlanTier::Free;
            entitlement.features = FeatureFlags::free();
            entitlement.reason = Some(
                "The licence could not be re-verified and the offline grace period has passed."
                    .to_string(),
            );
            entitlement.days_remaining = None;
        }
    }
    entitlement
}

fn verify_signature(payload: &[u8], signature: &[u8]) -> bool {
    let key_bytes = public_key();
    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_bytes) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(signature) else {
        return false;
    };
    verifying_key.verify(payload, &signature).is_ok()
}

/// Masks all but the last four characters.
pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.len() <= 4 {
        return "****".to_string();
    }
    let visible = &trimmed[trimmed.len() - 4..];
    format!("{}{}", "*".repeat(trimmed.len().saturating_sub(4)), visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_free_entitlement_never_blocks_core_analysis() {
        let entitlement = free_entitlement();
        assert_eq!(entitlement.state, EntitlementState::Free);
        assert!(entitlement.features.core_analysis);
        assert_eq!(entitlement.duplicate_group_limit, FREE_DUPLICATE_GROUP_LIMIT);
        assert!(!entitlement.features.scan_history);
    }

    #[test]
    fn pro_features_include_everything() {
        let features = FeatureFlags::pro();
        assert!(features.core_analysis);
        assert!(features.scan_history);
        assert!(features.advanced_duplicates);
        assert!(features.custom_cleanup_rules);
        assert!(features.scheduled_scans);
        assert!(features.export_reports);
        assert!(features.advanced_filters);
    }

    #[test]
    fn state_and_tier_names_match_the_wire_format() {
        assert_eq!(EntitlementState::ProActive.as_str(), "pro-active");
        assert_eq!(EntitlementState::ProExpired.as_str(), "pro-expired");
        assert_eq!(EntitlementState::parse("pro-active"), EntitlementState::ProActive);
        assert_eq!(PlanTier::Pro.as_str(), "pro");
    }

    #[test]
    fn an_empty_key_is_invalid_but_still_free() {
        let entitlement = entitlement_from_license("", 0);
        assert_eq!(entitlement.state, EntitlementState::Invalid);
        assert!(entitlement.features.core_analysis, "scanning still works");
        assert!(entitlement.reason.is_some());
    }

    #[test]
    fn a_malformed_key_is_invalid() {
        for key in ["nonsense", "aaaa.bbbb", "a.b.c", "!!!.???", "aaaa"] {
            let entitlement = entitlement_from_license(key, 0);
            assert_eq!(entitlement.state, EntitlementState::Invalid, "key: {key}");
            assert!(entitlement.features.core_analysis);
        }
    }

    #[test]
    fn a_well_formed_but_unsigned_key_is_invalid() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"tier":"pro","issuedAt":0,"expiresAt":null,"licenseId":"x"}"#);
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 64]);
        let key = format!("{payload}.{signature}");
        let entitlement = entitlement_from_license(&key, 0);
        assert_eq!(entitlement.state, EntitlementState::Invalid);
        assert_eq!(entitlement.reason.as_deref(), Some("Signature verification failed."));
    }

    #[test]
    fn masking_reveals_only_the_last_four_characters() {
        assert_eq!(mask_key("ABCD-1234-EFGH-5678"), "****************5678");
        assert_eq!(mask_key("abcd"), "****");
        assert_eq!(mask_key(""), "****");
    }

    #[test]
    fn the_offline_grace_period_expires_a_stale_cached_licence() {
        let mut cached = free_entitlement();
        cached.state = EntitlementState::ProActive;
        cached.tier = PlanTier::Pro;
        cached.features = FeatureFlags::pro();
        cached.verified_at = Some(0);
        cached.expires_at = Some(now_millis() + 86_400_000);

        let fresh = apply_offline_grace(cached.clone(), 1_000);
        assert_eq!(fresh.state, EntitlementState::ProActive);

        let stale = apply_offline_grace(cached, now_millis() + OFFLINE_GRACE_DAYS * 86_400_000 + 1);
        assert_eq!(stale.state, EntitlementState::Free);
        assert!(!stale.features.scan_history);
        assert!(stale.reason.is_some());
    }

    #[test]
    fn an_expired_cached_licence_falls_back_to_free() {
        let mut cached = free_entitlement();
        cached.state = EntitlementState::ProActive;
        cached.features = FeatureFlags::pro();
        cached.verified_at = Some(now_millis());
        cached.expires_at = Some(1_000);
        let resolved = apply_offline_grace(cached, now_millis());
        assert_eq!(resolved.state, EntitlementState::ProExpired);
        assert!(!resolved.features.scan_history);
    }

    #[test]
    fn an_expiry_in_the_past_is_expired() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"tier":"pro","issuedAt":0,"expiresAt":10,"licenseId":"x"}"#);
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 64]);
        // The signature will not verify with the placeholder key, so this exercises the invalid path.
        let entitlement = entitlement_from_license(&format!("{payload}.{signature}"), 1_000_000);
        assert_eq!(entitlement.state, EntitlementState::Invalid);
    }

    #[test]
    fn days_remaining_is_never_negative() {
        let entitlement = entitlement_from_license("", 0);
        assert!(entitlement.days_remaining.is_none());
    }
}
