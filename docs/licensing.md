# Licensing and entitlement

Diskwise is freemium. The design rule that shapes everything below: **basic usefulness must never
require payment**, and **a licence check must never block core scanning**.

## The free tier is a complete product

Free includes, with no account and no internet connection:

- Unlimited manual scans of any drive or folder.
- Sunburst, treemap and file-list views with full navigation.
- Large file finder with size, extension and age filters.
- Basic duplicate detection with a configurable group limit (25 groups by default).
- Cleanup suggestions for the folders you choose.
- Copy path and Reveal in File Explorer.
- Every safety guarantee: explicit confirmation, Recycle Bin by default, revalidation before acting.

## Entitlement states

| State | Meaning | Feature set |
| --- | --- | --- |
| `free` | Nothing activated. | Core analysis only. |
| `trial` | A trial licence is active. | Everything Pro. |
| `pro-active` | A valid, unexpired Pro licence. | Everything Pro. |
| `pro-expired` | The licence passed its expiry date. | Falls back to free. Scanning unaffected. |
| `invalid` | The key was malformed, badly signed, or revoked. | Falls back to free, with a reason shown. |

Transitions are validated (`ENTITLEMENT_TRANSITIONS` in `src/features/licensing/entitlements.ts`
and the equivalent in Rust), and every transition is written to `entitlement_events` with a reason.

## Where entitlement is decided

In Rust, in `src-tauri/src/licensing/verify.rs`. The frontend **reads** the entitlement object; it
never decides it. There is no `if (isPro)` scattered through the UI — feature gating goes through
`checkFeature` / `featuresFor`, which are pure functions with their own tests.

This matters for a specific reason: a boolean in the frontend is not a security boundary, and the
brief is explicit that validity must not be claimed from one. The webview is told which features are
available so it can render the right UI; the Rust backend enforces the actual limits (for example,
the duplicate group limit is applied where the report is produced, and the cleanup executor's
approved-roots check does not consult the entitlement at all).

## How a licence is verified

A key is `<base64url payload>.<base64url signature>`:

```json
{ "tier": "pro", "issuedAt": 1735689600000, "expiresAt": 1767225600000, "licenseId": "DW-XXXX" }
```

The signature is Ed25519 over the payload bytes, checked against a **public** key compiled into the
binary (`src-tauri/src/licensing/keys.rs`).

- The matching private key lives in the release process. It is not in this repository, not in the
  frontend bundle, and not in the packaged app.
- Verification is entirely offline. No network request is made, and none is needed.
- Any failure resolves to the free entitlement with a human-readable reason. The app keeps working.

## Offline policy

- The verified entitlement is cached in SQLite with the time it was verified.
- The cache is honoured offline for a 30-day grace period, then falls back to free.
- An expired cached licence moves to `pro-expired` immediately.
- Scanning, visualization and manual cleanup work in every one of those states.

To rotate the signing key, ship a build with a new public key. Old keys stop verifying and their
holders fall back to free — which is exactly the property you want from a revocation mechanism.

## Feature gates

`src/types/contracts.ts` → `FeatureFlags`:

| Flag | Free | Pro | What it unlocks |
| --- | --- | --- | --- |
| `coreAnalysis` | ✅ | ✅ | Sunburst, treemap, file list, large-file finder, manual navigation. Always true. |
| `scanHistory` | ❌ | ✅ | Multiple saved scans and historical comparison. |
| `advancedDuplicates` | ❌ | ✅ | Unlimited duplicate groups, batch review, byte-for-byte verification. |
| `customCleanupRules` | ❌ | ✅ | Cleanup rules beyond the built-in suggestions. |
| `scheduledScans` | ❌ | ✅ | Scheduled scans. |
| `exportReports` | ❌ | ✅ | CSV / JSON / HTML reports. |
| `advancedFilters` | ❌ | ✅ | Advanced type and age analytics. |

The free-tier duplicate limit is `duplicateGroupLimit` on the entitlement (25 by default, `0` for
unlimited). The UI applies it with `applyDuplicateLimit` and labels the truncation honestly rather
than hiding groups.

## Pricing experiments

The brief proposed testing these price points. **They are proposed experiments, not verified market
prices**, and the app says so on the licensing screen.

| Plan | Price | Notes |
| --- | --- | --- |
| Free | ₹0 | Forever. Everything listed above. |
| Pro trial | ₹0 for 14 days | Every Pro feature, no payment details. |
| Pro monthly | ₹149–249 | Proposed experiment. |
| Pro yearly | ₹999–1,499 | Proposed experiment. |
| Lifetime | — | **Not offered.** Only to be considered once support and update costs are known. |

A lifetime tier is the hardest one to price honestly (it commits to years of Windows updates), so it
is deliberately absent rather than guessed at.

## Payment provider readiness

The architecture is ready for a payment provider without any of it being wired up:

- `Entitlement`, `EntitlementEvent` and `LicenseActivationRequest` are already in the shared
  contract.
- `license_activate` / `license_deactivate` / `entitlement_events` are Rust commands with an
  offline-first implementation behind them.
- An online verification path would be a new command that *refreshes* the cached entitlement; the
  offline path stays as the fallback, so a provider outage can never lock anyone out of scanning.
- **No payment secret, API key or private signing key exists anywhere in the frontend or the
  packaged application.** Checkout belongs in a browser; the app only ever receives a licence key.

## Testing entitlement

`src/features/licensing/entitlements.test.ts` (24 tests) covers every state, the feature mapping,
legal and illegal transitions, key masking, the offline grace period and expiry.
`src-tauri/src/licensing/verify.rs` covers the same on the Rust side, including that an invalid key
still leaves `core_analysis` enabled. `src/lib/backend/demo/demoTransport.test.ts` proves a scan
completes with an invalid licence active.
