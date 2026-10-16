//! The directory walker.
//!
//! A bounded pool of workers pulls directories off a shared work stack. Each worker reads one
//! directory, records its files, and queues its sub-directories. The stack limits pending work on
//! deep trees; the worker count bounds open handles. Completion must be signalled by the *last*
//! worker, not by the thread joining them, or an empty directory deadlocks every scan.
//!
//! Safety rules enforced here:
//!   - `symlink_metadata` is used; reparse points are never traversed,
//!   - each directory is queued only once within the canonical scan root,
//!   - permission denials, missing paths, long paths and vanished files are recorded, not guessed,
//!   - cancellation is checked on every entry, so cancelling stops within one directory read.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::filesystem::long_path::normalize_long_path;
use crate::filesystem::{read_metadata, FileRecord, Kind};
use crate::platform;
use crate::scanner::progress::{ProgressCounters, ScanPhase};

/// Mirrors `ScanOptions` in `src/types/contracts.ts`. Tauri converts the camelCase keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanOptions {
    pub root_path: String,
    /// `0` = unlimited.
    pub max_depth: u32,
    pub follow_links: bool,
    pub include_hidden: bool,
    pub include_system: bool,
    /// `0` = unlimited.
    pub max_entries: u64,
    pub max_children_per_node: u32,
    pub compute_hashes: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            root_path: String::new(),
            max_depth: 0,
            follow_links: false,
            include_hidden: true,
            include_system: true,
            max_entries: 0,
            max_children_per_node: 100,
            compute_hashes: false,
        }
    }
}

/// One unreadable location. Mirrors `ScanErrorInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalkError {
    pub path: String,
    pub kind: String,
    pub message: String,
}

/// One directory in the flat table.
#[derive(Debug, Clone)]
pub struct DirectoryRecord {
    pub path: PathBuf,
    pub name: String,
    /// Index into `ScanRun::directories` of the parent, or `None` for the root.
    pub parent: Option<usize>,
    pub depth: usize,
    pub modified_at: Option<i64>,
    pub created_at: Option<i64>,
    /// The directory itself could not be read.
    pub access_denied: bool,
}

/// Everything a scan produced, before aggregation into a tree.
#[derive(Debug, Default)]
pub struct ScanRun {
    pub files: Vec<FileRecord>,
    /// Directory table. Index 0 is always the root.
    pub directories: Vec<DirectoryRecord>,
    pub errors: Vec<WalkError>,
    /// True when a safety cap (entry limit, cancellation) stopped the walk early.
    pub hit_limit: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

/// Shared work queue. Workers block on the condvar while the queue is empty but work remains.
struct WorkQueue {
    inner: Mutex<QueueState>,
    available: Condvar,
}

#[derive(Default)]
struct QueueState {
    pending: Vec<(PathBuf, usize)>,
    active: usize,
    finished: bool,
}

impl WorkQueue {
    fn with_root(root: PathBuf) -> Self {
        WorkQueue {
            inner: Mutex::new(QueueState {
                pending: vec![(root, 0)],
                active: 0,
                finished: false,
            }),
            available: Condvar::new(),
        }
    }

    fn push(&self, path: PathBuf, depth: usize) {
        if let Ok(mut state) = self.inner.lock() {
            if !state.finished {
                state.pending.push((path, depth));
                self.available.notify_one();
            }
        }
    }

    fn pop(&self) -> Option<(PathBuf, usize)> {
        let mut state = self.inner.lock().ok()?;
        loop {
            if state.finished {
                return None;
            }
            if let Some(item) = state.pending.pop() {
                state.active += 1;
                return Some(item);
            }
            if state.active == 0 {
                // Nobody is working and there are no pending directories: terminate all workers.
                state.finished = true;
                self.available.notify_all();
                return None;
            }
            state = self.available.wait(state).ok()?;
        }
    }

    fn complete(&self) {
        if let Ok(mut state) = self.inner.lock() {
            debug_assert!(state.active > 0);
            state.active -= 1;
            if state.active == 0 && state.pending.is_empty() {
                state.finished = true;
                self.available.notify_all();
            }
        }
    }

    fn finish(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.pending.clear();
            state.finished = true;
            self.available.notify_all();
        }
    }
}

/// Each popped directory is completed even if a worker returns early or unwinds.
struct WorkTicket<'a>(&'a WorkQueue);

impl Drop for WorkTicket<'_> {
    fn drop(&mut self) {
        self.0.complete();
    }
}

/// Per-worker output, merged once the walk completes.
#[derive(Default)]
struct WorkOutput {
    files: Vec<FileRecord>,
    directories: Vec<DirectoryRecord>,
    errors: Vec<WalkError>,
    denied: Vec<PathBuf>,
}

/// Runs a full walk. Blocking: call it from a worker thread, never on the async runtime.
pub fn walk(options: &ScanOptions, counters: &Arc<ProgressCounters>) -> ScanRun {
    let started = Instant::now();
    // Keep index/UI paths in their display form. Filesystem calls add the long-path prefix only
    // at the OS boundary; otherwise child paths from `read_dir` do not match the root in the index.
    let root = normalize_long_path(Path::new(&options.root_path));
    let mut run = ScanRun::default();

    // Root metadata. A missing or non-directory root is an honest error, not an empty scan.
    match read_metadata(&root, 0) {
        Ok(meta) if meta.kind == Kind::Folder => {
            run.directories.push(DirectoryRecord {
                path: root.clone(),
                name: volume_name(&root),
                parent: None,
                depth: 0,
                modified_at: meta.modified_at,
                created_at: meta.created_at,
                access_denied: false,
            });
        }
        Ok(_) => {
            counters.add_error();
            run.errors.push(WalkError {
                path: options.root_path.clone(),
                kind: "not-found".to_string(),
                message: "The scan root is not a directory.".to_string(),
            });
            run.hit_limit = true;
            return run;
        }
        Err(error) => {
            counters.add_error();
            run.errors.push(WalkError {
                path: options.root_path.clone(),
                kind: error_kind(&error),
                message: error.to_string(),
            });
            run.hit_limit = true;
            return run;
        }
    }

    if options.follow_links {
        counters.add_error();
        run.errors.push(WalkError {
            path: options.root_path.clone(),
            kind: "other".to_string(),
            message:
                "Following links/junctions is disabled until reparse-point safety is verified."
                    .to_string(),
        });
        run.hit_limit = true;
        return run;
    }

    let root_key = match canonical_key(&root) {
        Ok(key) => key,
        Err(error) => {
            counters.add_error();
            run.errors.push(WalkError {
                path: options.root_path.clone(),
                kind: error_kind(&error),
                message: format!("Cannot resolve the scan root: {error}"),
            });
            run.hit_limit = true;
            return run;
        }
    };
    counters.set_phase(ScanPhase::Walking);

    let queue = Arc::new(WorkQueue::with_root(root.clone()));
    let visited: Arc<Mutex<HashSet<PathBuf>>> =
        Arc::new(Mutex::new(HashSet::from([root_key.clone()])));
    let output: Arc<Mutex<WorkOutput>> = Arc::new(Mutex::new(WorkOutput::default()));

    let workers = worker_count();
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        let visited = Arc::clone(&visited);
        let output = Arc::clone(&output);
        let counters = Arc::clone(counters);
        let options = options.clone();
        let root_key = root_key.clone();

        handles.push(std::thread::spawn(move || {
            while let Some((dir, depth)) = queue.pop() {
                let _ticket = WorkTicket(&queue);
                if counters.is_cancelled() {
                    queue.finish(); // Wake workers already waiting on the condvar.
                    break;
                }
                if options.max_depth > 0 && depth >= options.max_depth as usize {
                    if counters.mark_truncated() {
                        counters.add_error();
                        record(&output, |out| {
                            out.errors.push(WalkError {
                                path: dir.to_string_lossy().to_string(),
                                kind: "limit-exceeded".to_string(),
                                message: format!(
                                    "Stopped at directory depth {}.",
                                    options.max_depth
                                ),
                            })
                        });
                    }
                    continue;
                }
                counters.set_current_path(&dir.to_string_lossy());
                scan_directory(
                    &dir, depth, &options, &counters, &queue, &visited, &output, &root_key,
                );
            }
        }));
    }

    for handle in handles {
        if handle.join().is_err() {
            counters.add_error();
            run.errors.push(WalkError {
                path: options.root_path.clone(),
                kind: "other".to_string(),
                message: "A scan worker failed unexpectedly; results may be incomplete."
                    .to_string(),
            });
            run.hit_limit = true;
        }
    }

    counters.set_phase(ScanPhase::Aggregating);

    if let Ok(mut collected) = output.lock() {
        collected.files.sort_by(|a, b| a.path.cmp(&b.path));
        collected.directories.sort_by(|a, b| a.path.cmp(&b.path));
        run.files = std::mem::take(&mut collected.files);
        run.directories
            .extend(std::mem::take(&mut collected.directories));
        run.errors.extend(std::mem::take(&mut collected.errors));

        // Mark directories that turned out to be unreadable.
        let denied: Vec<PathBuf> = std::mem::take(&mut collected.denied);
        for path in denied {
            if let Some(directory) = run.directories.iter_mut().find(|item| item.path == path) {
                directory.access_denied = true;
            }
        }
    }

    run.cancelled = counters.is_cancelled();
    run.hit_limit |= run.cancelled || counters.is_truncated();
    run.elapsed_ms = started.elapsed().as_millis() as u64;
    run
}

/// Reads one directory level.
/// These arguments are explicit per-worker guards and shared state; grouping them would obscure
/// the root-confinement, cancellation and quota checks at the security boundary.
#[allow(clippy::too_many_arguments)]
fn scan_directory(
    dir: &Path,
    depth: usize,
    options: &ScanOptions,
    counters: &ProgressCounters,
    queue: &WorkQueue,
    visited: &Mutex<HashSet<PathBuf>>,
    output: &Mutex<WorkOutput>,
    root_key: &Path,
) {
    // Recheck at dequeue time: a directory may have changed since it was queued.
    match canonical_key(dir) {
        Ok(key) if key.starts_with(root_key) => {}
        Ok(_) => {
            counters.add_error();
            record(output, |out| {
                out.errors.push(WalkError {
                    path: dir.to_string_lossy().to_string(),
                    kind: "other".to_string(),
                    message: "Directory resolved outside the chosen scan root; skipping."
                        .to_string(),
                })
            });
            return;
        }
        Err(error) => {
            counters.add_error();
            record(output, |out| {
                out.errors.push(WalkError {
                    path: dir.to_string_lossy().to_string(),
                    kind: error_kind(&error),
                    message: error.to_string(),
                })
            });
            return;
        }
    }
    let long_path = crate::filesystem::long_path::to_unc_long_path(dir);
    let entries = match std::fs::read_dir(&long_path) {
        Ok(entries) => entries,
        Err(error) => {
            counters.add_error();
            record(output, |out| {
                out.errors.push(WalkError {
                    path: dir.to_string_lossy().to_string(),
                    kind: error_kind(&error),
                    message: error.to_string(),
                });
                out.denied.push(dir.to_path_buf());
            });
            return;
        }
    };

    for entry in entries {
        if counters.is_cancelled() {
            return;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                counters.add_error();
                record(output, |out| {
                    out.errors.push(WalkError {
                        path: dir.to_string_lossy().to_string(),
                        kind: error_kind(&error),
                        message: error.to_string(),
                    });
                });
                continue;
            }
        };

        // `read_dir(\\?\C:\...)` returns `\\?\`-prefixed entries on Windows. Strip that
        // transport-only prefix before recording, indexing or queuing a child directory.
        let path = normalize_long_path(&entry.path());
        let meta = match read_metadata(&path, depth + 1) {
            Ok(meta) => meta,
            Err(error) => {
                // A file that vanished between `read_dir` and `stat` is normal on a live system.
                counters.add_error();
                record(output, |out| {
                    out.errors.push(WalkError {
                        path: path.to_string_lossy().to_string(),
                        kind: error_kind(&error),
                        message: error.to_string(),
                    });
                });
                continue;
            }
        };

        if meta.is_hidden && !options.include_hidden {
            continue;
        }

        // Links and reparse points are not indexed or traversed. The metadata probe above never
        // follows them; the command rejects requests to follow them until Windows is verified.
        if meta.is_symlink {
            continue;
        }

        if !counters.claim_entry(options.max_entries) {
            if counters.mark_truncated() {
                counters.add_error();
                record(output, |out| {
                    out.errors.push(WalkError {
                        path: dir.to_string_lossy().to_string(),
                        kind: "limit-exceeded".to_string(),
                        message: format!("Stopped after {} entries.", options.max_entries),
                    })
                });
            }
            queue.finish();
            return;
        }

        match meta.kind {
            Kind::File => {
                counters.add_file(meta.size);
                record(output, |out| {
                    out.files.push(FileRecord {
                        path: meta.path.clone(),
                        name: meta.name.clone(),
                        size: meta.size,
                        disk_size: meta.disk_size,
                        modified_at: meta.modified_at,
                        extension: meta.extension.clone(),
                        parent_path: dir.to_path_buf(),
                        depth: meta.depth,
                    });
                });
            }
            Kind::Folder => {
                let key = match canonical_key(&path) {
                    Ok(key) if key.starts_with(root_key) => key,
                    Ok(_) => {
                        counters.add_error();
                        record(output, |out| {
                            out.errors.push(WalkError {
                                path: path.to_string_lossy().to_string(),
                                kind: "other".to_string(),
                                message:
                                    "Directory resolved outside the chosen scan root; skipping."
                                        .to_string(),
                            })
                        });
                        continue;
                    }
                    Err(error) => {
                        counters.add_error();
                        record(output, |out| {
                            out.errors.push(WalkError {
                                path: path.to_string_lossy().to_string(),
                                kind: error_kind(&error),
                                message: error.to_string(),
                            })
                        });
                        continue;
                    }
                };
                let fresh = match visited.lock() {
                    Ok(mut guard) => guard.insert(key),
                    Err(_) => false,
                };
                if !fresh {
                    counters.add_error();
                    record(output, |out| {
                        out.errors.push(WalkError {
                            path: path.to_string_lossy().to_string(),
                            kind: "symlink-cycle".to_string(),
                            message:
                                "This directory was already visited; skipping to avoid a loop."
                                    .to_string(),
                        })
                    });
                    continue;
                }
                counters.add_folder();
                record(output, |out| {
                    out.directories.push(DirectoryRecord {
                        path: path.clone(),
                        name: meta.name.clone(),
                        parent: None,
                        depth: meta.depth,
                        modified_at: meta.modified_at,
                        created_at: meta.created_at,
                        access_denied: false,
                    });
                });
                queue.push(path, meta.depth);
            }
        }
    }
}

fn record<F>(output: &Mutex<WorkOutput>, apply: F)
where
    F: FnOnce(&mut WorkOutput),
{
    if let Ok(mut guard) = output.lock() {
        apply(&mut guard);
    }
}

fn error_kind(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "access-denied".to_string(),
        std::io::ErrorKind::NotFound => "not-found".to_string(),
        _ => {
            let message = error.to_string().to_lowercase();
            if message.contains("too long") || message.contains("filename is too long") {
                "long-path".to_string()
            } else {
                "io".to_string()
            }
        }
    }
}

/// Canonicalisation must succeed: a lexical fallback cannot establish root containment.
fn canonical_key(path: &Path) -> std::io::Result<PathBuf> {
    let long = crate::filesystem::long_path::to_unc_long_path(path);
    std::fs::canonicalize(&long)
}

/// Display name for a scan root: `C:\` becomes `Local Disk (C:)`, a folder becomes its name.
pub fn volume_name(root: &Path) -> String {
    let text = root.to_string_lossy().to_string();
    if text.len() == 3 && text.ends_with('\\') {
        return format!("Local Disk ({})", &text[..2]);
    }
    root.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or(text)
}

/// Worker pool size. Bounded so a pathological tree cannot exhaust handles.
pub fn worker_count() -> usize {
    let available = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    available.clamp(2, 16)
}

/// Drive letter for a path, when the path is on a drive-rooted volume.
pub fn drive_id_for(path: &Path) -> Option<String> {
    platform::drive_letter(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("diskwise-walk-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn an_empty_root_finishes_instead_of_waiting_for_idle_workers() {
        let root = temp_dir("idle");
        let (send, receive) = std::sync::mpsc::channel();
        let path = root.to_string_lossy().to_string();
        let handle = std::thread::spawn(move || {
            let counters = Arc::new(ProgressCounters::new());
            let run = walk(
                &ScanOptions {
                    root_path: path,
                    ..Default::default()
                },
                &counters,
            );
            send.send(run).unwrap();
        });
        let run = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("an empty scan deadlocked");
        handle.join().unwrap();
        assert_eq!(run.directories.len(), 1);
        assert!(!run.hit_limit);
        assert!(run.errors.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_fanout_finishes_after_every_worker_becomes_idle() {
        let root = temp_dir("fanout");
        for n in 0..32 {
            let folder = root.join(format!("sub-{n}"));
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("one.dat"), [42u8]).unwrap();
        }
        let (send, receive) = std::sync::mpsc::channel();
        let path = root.to_string_lossy().to_string();
        let handle = std::thread::spawn(move || {
            let counters = Arc::new(ProgressCounters::new());
            send.send(walk(
                &ScanOptions {
                    root_path: path,
                    ..Default::default()
                },
                &counters,
            ))
            .unwrap();
        });
        let run = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("parallel scan deadlocked");
        handle.join().unwrap();
        assert_eq!(run.files.len(), 32);
        assert_eq!(run.directories.len(), 33);
        assert!(run.errors.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_wakes_workers_blocked_on_empty_work() {
        let queue = Arc::new(WorkQueue::with_root(PathBuf::from("unused")));
        let _active = queue.pop().expect("root work");
        let other = Arc::clone(&queue);
        let (send, receive) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            send.send(other.pop()).unwrap();
        });
        queue.finish();
        assert!(receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .is_none());
        queue.complete();
        handle.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_outside_the_scan_root_is_not_traversed() {
        let root = temp_dir("escape");
        let outside = temp_dir("escape-target");
        fs::write(outside.join("private.txt"), [7u8]).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(
            &ScanOptions {
                root_path: root.to_string_lossy().to_string(),
                ..Default::default()
            },
            &counters,
        );
        assert!(run.files.is_empty());
        assert_eq!(run.directories.len(), 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn walks_a_small_tree_and_counts_everything() {
        let root = temp_dir("basic");
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        fs::write(root.join("a").join("b").join("deep.bin"), vec![0u8; 2048]).unwrap();
        fs::write(root.join("top.txt"), vec![0u8; 512]).unwrap();

        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            ..Default::default()
        };
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(&options, &counters);

        assert_eq!(run.files.len(), 2);
        assert_eq!(run.directories.len(), 3, "root + a + a\\b");
        assert_eq!(counters.files(), 2);
        assert_eq!(counters.folders(), 2);
        assert_eq!(counters.bytes(), 2048 + 512);
        assert!(run.errors.is_empty());
        assert!(!run.hit_limit);
        assert_eq!(run.directories[0].parent, None);
        assert!(run
            .files
            .iter()
            .any(|file| file.path == root.join("top.txt")));
        assert!(run
            .directories
            .iter()
            .any(|directory| directory.path == root.join("a").join("b")));
        // The scanner must not leak OS-only `\\?\` prefixes into UI/index paths.
        let index = crate::scanner::aggregate::ScanIndex::from_run(&run);
        assert_eq!(index.total_bytes(), 2560);
        assert!(index.position_of(&root.join("a").join("b")).is_some());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_a_missing_root_as_an_error() {
        let options = ScanOptions {
            root_path: "Z:\\definitely-not-here\\nope".to_string(),
            ..Default::default()
        };
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(&options, &counters);
        assert!(run.files.is_empty());
        assert!(!run.errors.is_empty());
        assert!(run.hit_limit);
    }

    #[test]
    fn honours_max_depth() {
        let root = temp_dir("depth");
        fs::create_dir_all(root.join("a\\b\\c")).unwrap();
        fs::write(root.join("a\\b\\c\\deep.bin"), vec![0u8; 16]).unwrap();

        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            max_depth: 2,
            ..Default::default()
        };
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(&options, &counters);
        assert!(run.files.is_empty(), "depth 2 never enters `c`");
        assert!(run.hit_limit);
        assert!(run
            .errors
            .iter()
            .any(|error| error.kind == "limit-exceeded"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn honours_max_entries() {
        let root = temp_dir("limit");
        for index in 0..50 {
            fs::write(root.join(format!("file-{index}.bin")), vec![0u8; 8]).unwrap();
        }
        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            max_entries: 10,
            ..Default::default()
        };
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(&options, &counters);
        assert_eq!(run.files.len(), 10, "the cap is atomic across workers");
        assert!(run.hit_limit);
        assert!(run
            .errors
            .iter()
            .any(|error| error.kind == "limit-exceeded"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellation_stops_the_walk_early() {
        let root = temp_dir("cancel");
        fs::create_dir_all(root.join("a")).unwrap();
        for index in 0..20 {
            fs::write(root.join(format!("a\\f{index}.bin")), vec![0u8; 4]).unwrap();
        }
        let counters = Arc::new(ProgressCounters::new());
        counters.cancel();
        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            ..Default::default()
        };
        let run = walk(&options, &counters);
        assert!(run.files.len() < 20, "cancellation must stop the walk");
        assert!(run.cancelled);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn every_file_records_its_parent_directory() {
        let root = temp_dir("parent");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub\\file.bin"), vec![0u8; 32]).unwrap();
        let options = ScanOptions {
            root_path: root.to_string_lossy().to_string(),
            ..Default::default()
        };
        let counters = Arc::new(ProgressCounters::new());
        let run = walk(&options, &counters);
        assert_eq!(run.files.len(), 1);
        assert!(run.files[0].parent_path.ends_with("sub"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn worker_pool_is_bounded() {
        assert!(worker_count() >= 2);
        assert!(worker_count() <= 16);
    }

    #[test]
    fn options_defaults_match_the_documented_contract() {
        let options = ScanOptions::default();
        assert_eq!(options.max_children_per_node, 100);
        assert!(!options.follow_links);
        assert!(options.include_hidden);
        assert!(options.include_system);
        assert_eq!(options.max_depth, 0);
        assert_eq!(options.max_entries, 0);
        assert!(!options.compute_hashes);
    }

    #[test]
    fn volume_name_formats_a_drive_root() {
        assert_eq!(volume_name(Path::new("C:\\")), "Local Disk (C:)");
        assert_eq!(volume_name(Path::new("D:\\Projects")), "Projects");
    }
}
