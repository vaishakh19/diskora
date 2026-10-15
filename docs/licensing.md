# Licensing and entitlement

> **Not for live sales.** As of 16 October 2026, paid checkout/issuance is incomplete and
> `create-checkout` rejects all non-test environments. The webhook does not acknowledge live
> `payment.succeeded`, and it issues **no licences**. Previously the webhook generated a random
> opaque key, stored only its hash and never delivered or signed a desktop-usable payload. Native
> activation does not yet connect purchase credentials to the service. All live purchase, delivery,
> refund and recovery instructions below are design goals, **not verified working features**. See
> [the release audit](release-audit.md).

The *planned* model is freemium with a **one-time purchase**. Two rules shape the design:

1. **Basic usefulness never requires payment.** Scanning, visualization, the large-file finder, basic
   duplicate detection and every safety guarantee work forever with no account and no internet
   connection.
2. **A licence check never blocks core scanning.** Every failure path resolves to the free tier with
   a human-readable reason, and the app keeps working.

---

## 1. The commercial model

One purchase, one licence, lifetime entitlement is the proposed model. **No live purchase is enabled.**

| | |
| --- | --- |
| Product | Diskwise Pro |
| Tier | `pro-lifetime` |
| Configured price | **₹1,499.00** (149,900 paise), not offered for live sale |
| Payment | Planned one-time purchase through Dodo Payments; live checkout disabled |
| Devices | **3 simultaneous activations** |
| Entitlement | Permanent — it never expires |
| Updates | Included for the life of the product |
| Offline lease | 30 days configured, but not enforced end-to-end (see §4) |

The current signed-payload builder can emit `leaseUntil: null` for a permanent tier. The 30-day
revocation window therefore **cannot be claimed as enforced** until the server signs a bounded
per-device lease and the native client refreshes it. The entitlement is permanent; a renewable
validation lease is not the purchase itself.

Those numbers live in exactly **one** place: `src-tauri/src/config/product.rs` (`DEFAULT_CONFIG`),
with the server-side twin in `supabase/migrations/0002_rls_and_functions.sql`. Nothing else in the
codebase may hardcode a price, a device limit or an entitlement period, and the config is validated
on load so a typo cannot silently grant access. Changing any of them is a reviewed change to those
two files, plus a rebuild and a redeploy.

### Permanent ownership vs. a temporary offline lease

These are two different things and the code keeps them apart deliberately:

- **`permanent: true`** — the *entitlement* never expires. A customer who paid once keeps the paid
  features for the life of the product. Nothing in this system silently ends it.
- **`leaseUntil`** — the *offline validation lease*: how long a previously verified licence keeps
  working with no re-validation. It exists so a refunded or chargebacked licence can be withdrawn
  without requiring an internet connection to keep working.

A lapsed lease on a permanent licence is reported as **"this licence needs to be re-verified; your
purchase is not affected"** — never as an expiry. The distinction is enforced in
`entitlement_from_payload`, `apply_offline_grace` and `Authorization::resolve`, and is asserted by
tests on both sides (`supabase/tests/licence-lifecycle.test.ts`,
`src-tauri/tests/licence_lifecycle.rs`, `src/features/licensing/entitlements.test.ts`).

---

## 2. The free tier is a complete product

Free, with no account and no internet connection:

- Unlimited manual scans of any drive or folder.
- Sunburst, treemap and file-list views with full navigation.
- Large-file finder with size, extension and age filters.
- Duplicate detection for the first **25 groups**, largest first, with real SHA-256 and byte
  verification.
- Cleanup **previews** for the folders you choose — every path, every size, before anything happens.
- Copy path and Reveal in File Explorer.
- Every safety guarantee: explicit confirmation, Recycle Bin by default, revalidation immediately
  before acting, partial-failure reporting.

The group limit is applied **in the backend**, where the report is produced, and the report totals
are recomputed exactly so the UI never shows a size the backend cannot account for. A modified
interface cannot ask for more.

---

## 3. Where entitlement is decided

**In Rust.** `src-tauri/src/authz/` is the only place a paid capability is granted.

```
src-tauri/src/
  config/product.rs     the single commercial configuration, validated on load
  authz/permissions.rs  the complete permission catalogue, free and paid
  authz/mod.rs          authorize() — the one gate every protected command goes through
  licensing/payload.rs  the signed payload: parse, verify, encode
  licensing/verify.rs   entitlement resolution, offline grace
  commands/*.rs         each protected command calls authorize() before doing anything
```

The frontend **reads** the entitlement object so it can render the right UI. It never decides
anything. Specifically:

- `cleanup_execute` rejects permanent deletion, verifies a one-use preview ticket for an exact set
  of scanned file paths and calls `authorize(PaidCleanup)`/`authorize(BulkFileOperations)` inside
  Rust. Browser-demo execution always refuses; Windows native safety still needs tests.
- `find_duplicates` applies the free group limit inside the blocking worker that builds the report.
- The executor's approved-roots check does not consult the entitlement at all: it is a filesystem
  safety property, not a commercial one, and it applies to every tier.

A modified React interface cannot bypass any of this by invoking a Tauri command directly, because
the command itself performs the check against an entitlement the backend resolved from a signed
payload. The webview contributes nothing to that proof.

### Permission catalogue

| Permission | Tier | What it gates |
| --- | --- | --- |
| `CoreAnalysis` | free | Scanning, aggregation, visualization, navigation |
| `FileInspection` | free | Metadata, Reveal in Explorer, open, copy path |
| `PreviewOnly` | free | Cleanup and duplicate previews |
| `LargeFileFinder` | free | The large-file finder, with free-tier limits |
| `BasicDuplicates` | free | Duplicate detection, with the free group limit |
| `PaidCleanup` | **paid** | Executing any cleanup plan |
| `PermanentDeletion` | **paid** | Bypassing the Recycle Bin |
| `DuplicateRemoval` | **paid** | Removing duplicate copies |
| `BulkFileOperations` | **paid** | Plans touching more than 50 paths |
| `ExportReports` | **paid** | CSV / JSON / HTML export |
| `ScheduledScans` | **paid** | Scheduled scans |
| `CustomCleanupRules` | **paid** | Rules beyond the built-in suggestions |
| `AdvancedFilters` | **paid** | Advanced type and age analytics |
| `ScanHistory` | **paid** | Saved scans and historical comparison |
| `UnlimitedDuplicates` | **paid** | Duplicate groups past the free limit |

`PERMISSIONS` in `authz/permissions.rs` is the complete list and the tests iterate it, so a new
capability cannot be added without deciding explicitly whether it is free or paid.

---

## 4. How a licence is verified

### The key format

A licence key is `<base64url(payload JSON)>.<base64url(Ed25519 signature)>`. The payload:

```json
{
  "schemaVersion": 1,
  "keyId": "k2026-01",
  "licenseId": "b1f2...",
  "productId": "diskwise-pro",
  "tierId": "pro-lifetime",
  "issuedAt": 1735689600000,
  "permanent": true,
  "leaseUntil": null,
  "deviceBinding": "dev-1a2b3c4d5e6f7a8b",
  "deviceLimit": 3,
  "metadata": {}
}
```

`schemaVersion` and `keyId` are what make rotation safe: an old licence keeps verifying after a new
key is introduced, and a licence signed by a key this build does not trust is refused with a message
that says to update rather than "invalid".

### Verification order

`payload::verify` runs these checks in this order, and each one fails closed:

1. **Shape** — is it `payload.signature` at all?
2. **Schema** — does this build understand `schemaVersion`?
3. **Key identity** — is `keyId` a trusted, active key? The signature is then checked against the
   key the payload *names*, so a payload cannot pick a weaker key.
4. **Signature** — Ed25519 over the exact payload bytes.
5. **Product** — does `productId` match this build's product?
6. **Tier** — is `tierId` a tier this build sells, and is it still available?
7. **Issued-at** — not more than 5 minutes in the future (clock skew).
8. **Device-limit sanity** — not zero, not absurd.
9. **Device binding** — does `deviceBinding` match this machine?
10. **Lease** — has `leaseUntil` passed?

Every error has a customer-facing `message()` that names no key id, no public key, no signature and
no internal path. A test asserts this for every fixture case.

### What is not a security boundary

The **frontend is not a security boundary**, and neither is anything it sends:

- a browser redirect from the payment provider,
- a query parameter on the return URL,
- an email address,
- a client-supplied order or payment id,
- an unverified webhook body.

None of these can grant an entitlement. Only a signature-verified `payment.succeeded` webhook does
that, and only a locally-verified signature can unlock a paid command.

### Device binding

`DeviceBinding::current(data_dir)` is `dev-` + an FNV-1a hash of
`hostname | OS install date | data-directory volume`.

**A machine identifier is not a secret.** It is a binding, not a credential: knowing it does not let
anyone use a licence, because the licence key is still required and the key is what the signature
covers. It is derived from stable machine properties so a reinstall on the same machine produces the
same binding — which is what makes a reinstall free (see §7).

The OS install date is read from the `InstallDate` registry value via `reg query`, rather than
pulling in a registry crate that could not be compile-checked in every environment.

---

## 5. The backend

Supabase Postgres + Edge Functions. Everything in `supabase/`.

### Schema

| Table | Written by | Purpose |
| --- | --- | --- |
| `products` | service role | The product being sold |
| `entitlement_configs` | service role | Price, currency, permanence, device limit, lease — the server-side twin of the Rust config |
| `orders` | service role | One row per payment, unique on `provider_payment_id` |
| `licences` | service role | One row per paid order, unique on `order_id`, key stored as SHA-256 |
| `device_activations` | service role | The authoritative device ledger |
| `webhook_events` | service role | Idempotency, keyed on `webhook-id` |
| `refunds` | service role | Provider refund references |
| `revocations` | service role | Every revocation with its reason |
| `audit_log` | service role | Append-only; ids, event names, short notes — never paths or keys |
| `recovery_tokens` | service role | Hashed, single-use, short-lived recovery tokens |
| `rate_limits` | service role | Fixed-window counters |

Row Level Security is enabled on every table and **no policy grants `INSERT`, `UPDATE` or `DELETE`
to `anon` or `authenticated`**. The desktop application does not read these tables directly at all:
it verifies licences offline with an embedded public key and calls Edge Functions that authenticate
with the service-role key. A licence key is a bearer token, so letting a client `SELECT` from
`licences` would turn any leaked anon key into a key dump.

### Edge Functions

| Function | Called by | Does |
| --- | --- | --- |
| `create-checkout` | the web | Builds the product cart from `entitlement_configs` and creates a Dodo checkout session. Grants nothing. |
| `dodo-webhook` | Dodo Payments | Verifies the signature, claims the id, then grants or revokes. The only granting path. |
| `licence-activate` | the desktop app | Looks the key up by hash, enforces the device limit atomically, mints and returns a signed payload. |
| `licence-deactivate` | the desktop app | Releases the slot for the calling device only. |
| `licence-recover` | the web / desktop | Two-step, account-independent recovery (§7). |

### Dodo Payments integration

Dodo follows the **Standard Webhooks** specification. The details that are easy to get wrong, all
verified against the current official documentation:

- Headers: `webhook-id`, `webhook-signature`, `webhook-timestamp`.
- The signed content is `"{webhook-id}.{webhook-timestamp}.{rawBody}"`, joined by periods.
- The HMAC-SHA256 key is the **base64 decoding** of the webhook secret **with the `whsec_` prefix
  stripped**. Passing the raw `whsec_...` string produces signatures that never verify.
- `webhook-signature` is a space-separated list of `v1,<signature>` entries; any one verifying is
  sufficient.
- The raw body must be the exact bytes Dodo sent. Re-serialising parsed JSON changes the key
  whitespace and breaks the signature, so every handler reads `request.text()` once.
- Dodo retries until it receives a 2xx, so every handler must be idempotent.
- The documentation is explicit that **the browser redirect is not the source of truth** — the
  webhook is.

Test checkout calls Dodo's test endpoint with a server-built cart. **Live checkout returns 503**;
no production payment should be taken until the signed-device-grant lifecycle, customer delivery,
provider-product/amount verification and reconciliation have been tested with the real provider.

### Idempotency and duplicate delivery

The handler verifies the signature, then uses `claim_webhook_v2` in migration `0004`:

1. A new id/body is claimed with an attempt token. A different body using the same id is refused.
2. An *already finished* identical delivery gets `2xx {duplicate: true}`; a still-running delivery
   gets 503, so the provider retries rather than silently losing work.
3. A failed claim is retriable immediately; a crashed worker's claim can be retried after a five-minute
   lease. Only the worker holding the current token may mark it finished.
4. Processing and `finish_webhook_v2` must both succeed **before** the 2xx response. Retries can
   repeat intermediate writes, so each paid mutation still needs its own idempotency key.

The old boolean claim functions remain present for a staged deployment but must not be called by
new code. These SQL semantics have been exercised in PGlite and are checked in the PostgreSQL CI
workflow; **no Dodo delivery has been executed against a deployed Edge Function**.

### Test mode cannot grant a production entitlement

The shipped `DEFAULT_CONFIG` has `environment: Test` and an all-zero placeholder trusted key,
checked by CI. The checkout function reads `DODO_ENVIRONMENT` on the server and refuses any value
other than `test`. Signed test-mode payments with explicit `environment: test` metadata can be
recorded as test orders, but **the webhook creates no licence in either environment**. Live or
unknown-environment successes return 503 without claiming the delivery, so operations can reconcile
any earlier live payment instead of mistaking an undeliverable key for fulfilment.

**Changing an environment variable alone cannot enable live sales.** The required engineering and
external verification are listed in §9.

---

## 6. Refunds, chargebacks and revocation

**Design, not a deployment guarantee.** The handler now returns 503 if a revocation cannot find
its order, validate its refund id/amount, or persist every write; it never acknowledges a failed
revocation as complete. Dodo's actual refund/dispute payload shapes, partial-refund policy,
concurrent delivery order and real Supabase Edge deployment still need end-to-end tests and
operator reconciliation. Do not sell live licences until those checks pass.

| Event | Intended effect |
| --- | --- |
| `refund.succeeded` | A `refunds` row, the order moves to `refunded`, the licence moves to `refunded` with `status_reason = 'refund'`, and a `revocations` row records why. |
| `dispute.accepted` / `dispute.lost` | Same, with `source = 'dispute'`. |
| `dispute.opened` / `challenged` / `expired` / `won` / `cancelled` | Recorded in `webhook_events`, acknowledged, and **deliberately ignored**: a challenge is not a loss. |
| `payment.failed` / `processing` / `cancelled` / `charge_failed` | Acknowledged and ignored. No entitlement. |
| `refund.failed` | Acknowledged and ignored: the refund did not happen. |

A revoked licence is **kept, never deleted**, and `revocations` preserves the reason. The desktop app
refuses activation for any status other than `active`, and the offline lease bounds how long a
previously-activated licence keeps working after a revocation.

---

## 7. Device limit, transfer and recovery

### The limit is enforced atomically, in the database

`activate_device()` in `supabase/migrations/0002_rls_and_functions.sql` takes `SELECT ... FOR UPDATE`
on the licence row, counts the active devices, and only then inserts. Two concurrent activations
queue behind each other, so the count cannot be read twice at the same value. A check-then-insert in
application code would not be safe, and is not what this does.

The CI `migrations` job seeds a licence with a limit of one, activates it twice against a real
Postgres, and asserts the second activation raises `check_violation`.

### Reinstall, hardware change and transfer

| Situation | Binding | Outcome |
| --- | --- | --- |
| Reinstall on the same machine | unchanged | The activation row is **refreshed**. No slot consumed. |
| Replace the machine | new | A new activation. Must be within the limit. |
| Move the licence deliberately | new | Deactivate the old device first (§8), then activate the new one. |

Recovery is **account-independent**: a customer who lost their key does not need to remember which
email they used, and does not need to pay again.

1. `POST licence-recover {action: "request", email, orderRef?}` — a single-use, 30-minute recovery
   token is created; only its SHA-256 is stored. The response is identical whether or not the email
   matches an order, so the endpoint cannot be used to enumerate customers.
2. The link is delivered out of band. The transport is behind one function,
   `deliverRecoveryLink`, which is **not implemented in this repository** — there is no mail
   credential here and the code does not pretend to send anything. See §9.
3. `POST licence-recover {action: "claim", token, deviceBinding}` — the token is consumed with a
   conditional update (so a double-submit cannot mint twice), the device is activated through the
   same atomic path, and a fresh signed payload is returned.

The token is rate-limited, capped at 10 attempts, and a revoked licence is never recoverable.

---

## 8. Using the app

**Activate.** Licensing → paste the key → Activate. The key is verified entirely offline. Nothing is
uploaded: no key, no machine identifier, no file paths.

**See your devices.** Licensing → Devices. This is a local mirror of the service's ledger, so it
works offline and shows exactly what the app can see. The service remains authoritative.

**Deactivate this device.** Licensing → Deactivate. Releases the slot locally and asks the service to
release it too. Only the device you are using can be deactivated from here — releasing somebody
else's device would be a way to defeat the limit, and that goes through the account page.

**Re-verify.** Connect to the internet and re-activate. This refreshes the lease. It is never
required to keep scanning.

---

## 9. Operations

### Generating a signing key

```bash
node scripts/licence-keys.mjs keygen k2026-01
```

The script prints the **public** key as base64 (paste it into
`CommercialConfig::trusted_keys` in `src-tauri/src/config/product.rs`) and never prints the private
key. A lost private key is rotated, not recovered.

Store the private key in the licence service's secret store:

```bash
supabase secrets set LICENCE_SIGNING_PRIVATE_KEY="$(cat key.pem)"
```

### Secrets

| Secret | Where | Committed? |
| --- | --- | --- |
| `LICENCE_SIGNING_PRIVATE_KEY` | Supabase secret | **Never** |
| `DODO_PAYMENTS_API_KEY` | Supabase secret | **Never** |
| `DODO_WEBHOOK_SECRET` | Supabase secret | **Never** |
| `SUPABASE_SERVICE_ROLE_KEY` | Supabase secret | **Never** |
| `LICENCE_SIGNING_KEY_ID` | Supabase secret | Yes, it is not a secret |
| `DODO_ENVIRONMENT` | Supabase secret | Yes |
| `RECOVERY_EMAIL_PROVIDER` | Supabase secret | Yes |

None of these appear in the app, the frontend bundle, the installer, this repository, the logs or
any crash report. The desktop build carries **only the public key**, which is not a secret. CI greps
the built bundle and the tracked code for the shapes these secrets take and fails if it finds one.

### Configuring recovery delivery

`deliverRecoveryLink` in `supabase/functions/licence-recover/index.ts` is the single place the
recovery email is sent. It currently checks for `RECOVERY_EMAIL_PROVIDER` and, when it is unset,
logs and returns without sending — which means recovery is **not functional out of the box**. Setting
up a mail provider and filling in that function is a deployment task, documented here rather than
faked.

### Creating a Dodo test product

1. Sign in to the Dodo Payments dashboard and switch to **Test mode**.
2. Create a one-time product, note its `product_id`.
3. Set `DODO_PRODUCT_PRO_LIFETIME` to that id in the Supabase secrets.
4. Add a webhook endpoint pointing at `https://<project>.functions.supabase.co/dodo-webhook` and
   subscribe it to the events listed in §6.
5. Copy the signing secret into `DODO_WEBHOOK_SECRET`. It arrives as `whsec_...`; the code strips the
   prefix and base64-decodes the rest.

### Making a test purchase

Use the test-mode checkout, complete the payment, and watch `dodo-webhook` in the Supabase logs. A
`payment.succeeded` in test mode records an order and issues **no licence** — that is the guard in
§5 working. To exercise the granting path locally, mint a licence with
`node scripts/licence-keys.mjs mint --device <binding>` using a throwaway key pair, and verify it
with `node scripts/licence-keys.mjs verify`.

### Switching to live mode — BLOCKED

**Do not set `DODO_ENVIRONMENT=live` and accept payments yet.** The current checkout deliberately
returns 503 for non-test environments and the webhook will not acknowledge a live payment success.
This is a release blocker, not an operator toggle. Before any code change removes those guards:

1. Build a server-verified Dodo payment reconciliation path that checks the provider's actual
   payment state, environment, product, amount and currency against the configured tier; test
   signed failures, replay, refund and out-of-order events on a disposable deployment.
2. Implement **signed** purchase-to-device grants and a real customer delivery/recovery channel.
   The current random purchase key cannot be parsed by the native signed-payload verifier, and
   neither an email credential nor a working recovery transport is included here. Keep the private
   key only in a server secret store; embed only its reviewed public key in the Windows build.
3. Connect native activation/deactivation to the verified service and issue bounded, renewable
   per-device leases; demonstrate three-device enforcement, reinstalls, transfers and revocations
   on Windows. A permanent purchase must not be confused with a never-expiring offline lease.
4. Apply and test migrations/role grants in the intended Supabase project, deploy the Edge
   Functions in the right order, and verify a full sandbox checkout through receipt, signed
   activation, expiry and refund. Obtain independent authorization for a tiny real-money test;
   never use real customer payments as a diagnostic.
5. Only after the native build, unsigned/signed installer policy, NTFS safety and all other P0
   release gates pass should a reviewed change deliberately enable live mode.

The actual Dodo account, Supabase credentials, signing certificate, delivery provider and release
approval are external/manual requirements; none are generated or committed by this repository.

### Key rotation and compromise recovery

**Rotation.** Add the new key with `active: true` and a new `key_id`; set the old one to
`active: false`. Both verify; new licences carry the new `key_id`. Because `key_id` travels in the
payload, a licence signed by a retired key is told apart from one signed by an untrusted key.

**Compromise.** If the private key leaks, an attacker can mint licences that verify offline. There is
no way to un-sign them, and it would be dishonest to claim otherwise:

1. Set the compromised key to `active: false` and ship a build. Existing licences fall back to free
   with "update Diskwise".
2. Issue replacement licences under a new key to every affected customer, using the recovery flow —
   no one pays twice.
3. Audit `audit_log` and `device_activations` for activations that do not correspond to real
   customers.

**Honest limitation.** Local signature verification is **not** DRM. It stops a casual edit of a
settings file and it makes forging a licence require the private key. It does not prevent copying the
app, patching the binary, or reverse engineering it, and it is not represented as doing so.

### Backup and disaster recovery

- **Postgres** — Supabase's daily backups plus Point-in-Time Recovery. `licences` and
  `device_activations` are the only irreplaceable state: everything else can be rebuilt from the
  payment provider's records.
- **The signing key** — stored in the secret store only. Losing it means rotating, not recovering.
- **The desktop app** — if the service is unreachable, activation and recovery fail with "activation
  is unavailable right now". Previously activated customers are unaffected for the length of their
  offline lease, and scanning never stops.

---

## 10. Testing

| Test | Where | Runs |
| --- | --- | --- |
| Valid licence verifies | `supabase/tests/licence-lifecycle.test.ts`, `src-tauri/tests/licence_lifecycle.rs` | local + CI |
| Invalid signature refused | both | local + CI |
| Modified payload refused | both | local + CI |
| Wrong product / wrong device / unknown tier / unknown schema refused | both | local + CI |
| Lapsed lease refused, live lease accepted | both | local + CI |
| Absurd device limit refused | both | local + CI |
| Malformed and empty keys refused | both | local + CI |
| Webhook signature verification | `supabase/tests/licence-lifecycle.test.ts` | local + CI |
| Webhook replay with a stale timestamp refused | same | local + CI |
| Webhook replay with an edited body refused | same | local + CI |
| `whsec_` prefix handling | same | local + CI |
| Claim retry, changed-body refusal and stale-worker lease | `claim_webhook_v2` / `finish_webhook_v2`; CI `migrations` job | PGlite locally; real Postgres pending |
| Device limit enforced atomically | CI `migrations` job against a real Postgres | CI |
| No client write policy exists | CI `migrations` job | CI |
| RLS enabled on every licensing table | CI `migrations` job | CI |
| A forged licence grants nothing | `src-tauri/tests/licence_lifecycle.rs` | CI |
| A modified frontend cannot bypass authorization | `src-tauri/tests/licence_lifecycle.rs` | CI |
| A permanent licence with a lapsed lease stays paid | all three suites | local + CI |
| No secret in the bundle or the repo | CI `frontend` and `audit` jobs | CI |
| The shipped trusted key is the placeholder | CI `licence` job | CI |

**What has not been run.** There is no local Rust toolchain. In the 16 October CI run,
Windows `cargo fmt --check` passed, Clippy failed and native tests/build/installers were skipped.
Initial migrations passed against CI's real Postgres; migration `0004` has only run in PGlite with
a `pgcrypto` substitution and awaits the revised CI job. No Edge Function has been deployed to a
live Supabase project or tested against real Dodo events. The Node contract suite verifies shared
HMAC/signing helpers but **not** paid issuance, delivery, refund, recovery or native activation.

---

## 11. Where the numbers live

| Value | Location |
| --- | --- |
| Product id | `DEFAULT_CONFIG.product_id` and `products.id` |
| Tier id, name, price, currency, permanence, update eligibility | `DEFAULT_CONFIG.tiers[*]` and `entitlement_configs` |
| Device limit | `DEFAULT_CONFIG.device_limit`, `entitlement_configs.device_limit`, and the signed payload |
| Offline lease days | `DEFAULT_CONFIG.offline_lease_days` and `entitlement_configs.offline_lease_days` |
| Free duplicate group limit | `DEFAULT_CONFIG.free_duplicate_group_limit` |
| Trusted signing keys | `DEFAULT_CONFIG.trusted_keys` |
| Payload schema version | `DEFAULT_CONFIG.license_schema_version` and `licences.schema_version` |
| Environment | `DEFAULT_CONFIG.environment` and `DODO_ENVIRONMENT` |

Nothing else may contain any of these as a literal. If you find one, it is a bug.
