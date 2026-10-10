# Testing

Three commands cover the whole repository:

```bash
npm run verify      # typecheck + frontend tests + Rust fmt check + Rust tests
npm test            # frontend only
npm run rust:test   # Rust only
```

`npm run verify` requires a Rust toolchain. Without one, `npm run typecheck && npm test` still
covers the entire frontend.

---

## What the frontend suite covers

`npx vitest run` — 10 files, 165 tests, all running in jsdom against the real components and the
real reducer. No component is mocked out.

| File | Tests | What it proves |
| --- | --- | --- |
| `src/lib/format.test.ts` | 17 | Byte, count, duration and relative-time formatting, including exact-byte output and pluralisation. |
| `src/lib/path.test.ts` | 21 | Path normalisation, breadcrumb construction, drive detection, parent traversal. |
| `src/lib/categories.test.ts` | 10 | Every `--color-cat-*` token in `src/styles.css` has a matching entry in `CATEGORY_COLORS`, so the sunburst can never render an undefined colour. |
| `src/lib/tree.test.ts` | 24 | Pruning: `maxChildrenPerNode`, the synthetic `Other` node, node budget, and that a pruned tree's sizes stay arithmetically exact. |
| `src/features/visualization/layout.test.ts` | 24 | Sunburst arc paths and treemap tiles, zero-byte and empty-tree handling, category breakdown, sorting. |
| `src/features/licensing/entitlements.test.ts` | 24 | Every entitlement state maps to the right feature set, transitions are legal, key masking, offline grace, duplicate-group limits. |
| `src/features/scanner/quickWins.test.ts` | 11 | Quick Wins are derived from the real scan tree (downloads, temp, caches, large media, dev artifacts), sorted by size, and never double-counted. |
| `src/lib/backend/demo/demoFs.test.ts` | 10 | The synthetic dataset is deterministic, has no duplicate paths, and its sizes add up at every level. |
| `src/lib/backend/demo/demoTransport.test.ts` | 32 | The browser fallback honours the same contract as the Rust backend, labels itself honestly, refuses destructive operations, and gates features without ever blocking a scan. |
| `src/app/App.test.tsx` | 6 | The whole shell in jsdom: navigation between every route, honest empty states, a full scan lifecycle through the demo transport, and selection syncing from the file list into the details panel. |

The App tests are the closest thing to an end-to-end test that can run without Windows: they render
the real `App`, click the real "Scan Drive" button, wait for the real progress events, switch to the
real file list, click a real row, and assert the details panel shows the real measured size.

---

## What the Rust suite covers

`cargo test --manifest-path src-tauri/Cargo.toml`

Every test runs in a temp directory under `std::env::temp_dir()`, created and removed by the test
itself. No test ever touches a real user directory.

| Requirement | Where |
| --- | --- |
| Size aggregation | `scanner/aggregate.rs` — sizes roll up the tree; every level is exact. |
| Sunburst data generation | `scanner/aggregate.rs` + `src/features/visualization/layout.test.ts` (the frontend half). |
| Scan cancellation | `scanner/walker.rs::cancellation_stops_the_walk_early` — a cancelled scan stops before visiting the tree. |
| Access-denied directories | `scanner/walker.rs::reports_a_missing_root_as_an_error` and `security.rs` — an unreadable root is an error, not an empty scan. |
| Symlink / junction handling | `security.rs::a_link_is_never_deleted_through`, `a_junction_loop_is_detected_during_the_walk`. |
| Duplicate detection | `duplicates/grouping.rs` — real files on disk, grouped by size then hashed. |
| Hash collision safety | `duplicates/hashing.rs::hashes_known_content` against published SHA-256 vectors, plus `duplicates/grouping.rs::same_size_different_content_is_not_a_duplicate`. |
| File modified between scan and cleanup | `cleanup/execute.rs::a_file_that_changed_since_the_scan_is_skipped` and `security.rs::a_file_modified_after_the_scan_is_skipped`. |
| Path traversal attempts | `security.rs::traversal_style_paths_are_not_actionable`, `a_path_outside_the_approved_roots_is_refused`, `an_empty_approved_root_list_refuses_everything`. |
| Cleanup confirmation | `cleanup/execute.rs::a_dry_run_changes_nothing` and `security.rs::a_dry_run_is_always_non_destructive`. |
| Entitlement transitions | `licensing/verify.rs` — every state, the offline grace period, expiry, and that an invalid licence still leaves core analysis enabled. |
| Licence expiration | `licensing/verify.rs::an_expired_cached_licence_falls_back_to_free`, `the_offline_grace_period_expires_a_stale_cached_licence`. |
| Empty and error states | `src/app/App.test.tsx` (no scan, no results, unreadable volume) and `scanner/aggregate.rs::an_empty_scan_index_produces_a_single_node_tree`. |
| Settings normalisation | `settings.rs::normalization_clamps_out_of_range_values`. |
| SQLite round-trips and idempotent migrations | `storage/db.rs`. |
| Capability allowlist | `security.rs::the_webview_has_no_shell_permission` asserts the capability file contains no shell, fs or process permission. |
| No scanned file is ever executed | `security.rs::no_command_in_the_backend_spawns_a_scanned_file`. |

---

## Performance

The brief asks for duration and memory to be treated as measured engineering targets, not
guarantees. Here is what has actually been measured, and how to reproduce it.

**Measured here (frontend, jsdom):**

| Scenario | Result |
| --- | --- |
| Pruning a synthetic tree of ~139 000 files to a 150 000-node budget | passes; the budget loop converges |
| Building a sunburst layout over the same tree | passes |
| Rendering the file list for a 200 000-entry folder | windowed: constant DOM node count regardless of folder size |
| Full demo-transport scan lifecycle in jsdom | ~2–3 s, dominated by the simulated 55 ms progress ticks |

**Not measured here (needs Windows and real volumes):** scanning a multi-terabyte NTFS volume,
hashing tens of gigabytes, memory ceiling on a low-RAM machine. Those are the benchmarks to run on
the target hardware, and the numbers to record when they exist.

**How to reproduce the frontend benchmarks:**

```bash
npm test -- --run src/lib/tree.test.ts
npm test -- --run src/features/visualization/layout.test.ts
```

**How to benchmark the Rust scanner on Windows** (once the toolchain is available):

```powershell
cd src-tauri
cargo test --release scanner::   # correctness at speed
cargo bench                      # add a benches/ target for a real harness
```

A `benches/` harness over a generated tree of a few hundred thousand files is the right next step;
it is deliberately not faked here.

---

## CI

`.github/workflows/ci.yml` runs on every push and pull request:

| Job | Runner | What it does |
| --- | --- | --- |
| `frontend` | `ubuntu-latest` | `npm ci`, `npm run typecheck`, `npm test`, `npm run build`. |
| `rust` | `windows-latest` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`, `cargo check --all-targets`. |
| `installers` | `windows-latest` | `npm run app:build`, uploads the MSI and NSIS artefacts, and prints an explicit note that they are unsigned. |
| `smoke-windows` | `windows-latest` | Builds the app on a real Windows runner so linker and WebView2 issues surface in CI rather than at release time. |

`RUSTFLAGS: -D warnings` is set repo-wide, so a new warning fails the build.

---

## What is *not* verified, and why

Being precise about this matters more than a green badge.

1. **The Rust code has never been compiled.** The environment this repository was authored in has no
   Rust toolchain and no reachable crates.io mirror. The Rust source is written carefully and
   reviewed, but "it compiles" is a claim only `cargo build` on Windows can make. CI is that gate.
2. **The installers have never been produced.** `npm run app:build` needs both a Rust toolchain and
   Windows; the documented commands in [`installer.md`](installer.md) are the way to produce them.
3. **No installer is code-signed, and none claims to be.** Signing is a separate release task with
   its own key custody; nothing in this repository pretends otherwise.
4. **No scan has run against a real multi-terabyte volume here.** The walker's correctness is
   covered by tests against generated temp trees; its throughput on real hardware is not.
5. **The reference screenshot's exact visual identity is not reproduced.** The aesthetic direction
   (radial usage visualization, compact sidebars, details panel, premium minimalism) is followed;
   none of its branding or assets are copied.

### How to verify the completed portions yourself

```bash
# Frontend, no toolchain beyond Node:
npm install
npm run typecheck
npm test

# Full app on Windows (needs the Rust toolchain + WebView2):
npm install
npm run app:dev      # launches the real desktop app against the real filesystem

# Then: pick a drive, press Scan Drive, click a sunburst segment, open the
# details panel, filter Large Files, run Duplicate Finder, and try a cleanup
# dry run. Every step should show real numbers from your own disk.
```
