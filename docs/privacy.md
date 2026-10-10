# Privacy

Diskwise is local-first. There is no account, no mandatory cloud, no automatic uploads and no
background scanning. This page states exactly what is stored and what leaves the device; the running
app shows the same list, read from the backend rather than from marketing copy
(`privacy_report` command, rendered in the Privacy screen).

## Stored locally

One SQLite database at `%APPDATA%\com.diskwise.app\diskwise.sqlite3`:

| Table | Contents |
| --- | --- |
| `settings` | Theme, default scan root, children-per-folder, link following, cleanup confirmation preference, Recycle Bin preference, age threshold, cache and temp folders, never-touch paths, crash-reporting opt-in, duplicate group limit. |
| `scan_history` | One row per scan: root path, root name, start and finish timestamps, duration, status, total bytes, file count, folder count, error count, whether it was truncated, drive id. **No file names, no individual paths.** |
| `entitlement_cache` | The current licence and its entitlement state, plus when it was last verified. |
| `entitlement_events` | A short log of entitlement transitions with a reason. |
| `cleanup_rules` | The cleanup rules the user configured. |

Scan results themselves live in memory only. They are never written to disk, and the tree sent to
the webview is pruned — see [`architecture.md`](architecture.md#tree-pruning--the-dont-ship-millions-of-nodes-contract).

Deleting the database is safe. Diskwise starts from a clean slate and scanning behaves identically.

## Leaves the device

**Nothing, by default.** The app makes no network request during a scan, a duplicate search or a
cleanup.

Two exceptions, both opt-in and both off unless you turn them on:

1. **Crash reporting** (Settings → Privacy). When enabled, a crash sends a stack trace and the app
   version. No file paths, no file names, no scan results, no machine identifier beyond what the
   transport itself carries.
2. **Licence re-validation.** Disabled by default. When it is ever enabled it would send only the
   licence key, never anything about your filesystem. Entitlement is verified offline by default and
   works with no network at all.

There is no analytics on full paths. There is no telemetry about which folders you scan. There are
no ads and no sponsored results, and no code path that could produce either.

## What Diskwise will not do

- Scan in the background. A scan runs while you are looking at it and stops when you cancel it.
- Execute a file it has analysed.
- Elevate privileges. Scanning a folder you can already read needs no administrator rights, and the
  app never asks for them.
- Delete a file because it was selected or analysed.
- Classify your documents, browser profiles, development dependencies or package caches as junk.
- Offer Windows system files or application data for deletion.

## Permissions the webview has

`src-tauri/capabilities/default.json` grants the webview:

```
core:default
core:window:allow-minimize, allow-toggle-maximize, allow-close, allow-start-dragging,
             allow-is-maximized, allow-set-focus
core:event:default
dialog:allow-open, allow-message, allow-ask, allow-confirm
opener:allow-open-path, allow-open-url
```

That is the complete list. There is no `shell` permission, no filesystem scope and no ability to
spawn a process. Every filesystem action — reading metadata, revealing in Explorer, opening a file,
moving something to the Recycle Bin — is a narrowly-scoped Rust command that validates its input
first. `src-tauri/src/security.rs` contains a test that fails if a dangerous permission is ever added
to the capability file.

## Verifying this yourself

The claims above are testable rather than aspirational:

- `src-tauri/src/security.rs::the_webview_has_no_shell_permission` asserts the capability file's
  contents.
- `src-tauri/src/security.rs::no_command_in_the_backend_spawns_a_scanned_file` asserts the only
  process the backend ever starts is `explorer.exe` with a fixed argument shape.
- The Privacy screen in the running app reads the real database and data-directory paths from the
  backend, so the list cannot drift from reality.
