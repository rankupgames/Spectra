use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use spectra_core::{IndexReport, is_supported_path, sync_project};

const DEFAULT_DEBOUNCE_MS: u64 = 2_000;
const MIN_DEBOUNCE_MS: u64 = 100;
const MAX_DEBOUNCE_MS: u64 = 60_000;

#[derive(Clone)]
pub(crate) struct AutoSync {
    inner: Arc<Mutex<AutoSyncInner>>,
    debounce: Duration,
}

impl Default for AutoSync {
    fn default() -> Self {
        Self::with_debounce(configured_debounce())
    }
}

impl AutoSync {
    fn with_debounce(debounce: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(AutoSyncInner::default())),
            debounce,
        }
    }

    pub(crate) fn ensure_project(&self, project: &Path) -> SyncSnapshot {
        let root = match project.canonicalize() {
            Ok(root) if root.is_dir() => root,
            Ok(root) => {
                return SyncSnapshot::degraded(format!("{} is not a directory", root.display()));
            }
            Err(error) => return SyncSnapshot::degraded(error.to_string()),
        };

        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(project) = inner.projects.get_mut(&root) {
            let watcher_degraded = !project
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active;
            if watcher_degraded {
                project.watcher.take();
            }
            if project.watcher.is_none() {
                match ProjectWatcher::start(root.clone(), self.debounce, project.status.clone()) {
                    Ok(watcher) => project.watcher = Some(watcher),
                    Err(error) => {
                        let mut state = project
                            .status
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.active = false;
                        state.last_error = Some(error.to_string());
                    }
                }
            }
            return snapshot(&project.status);
        }

        let status = Arc::new(Mutex::new(SyncState::default()));
        let watcher = match ProjectWatcher::start(root.clone(), self.debounce, status.clone()) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                let mut state = status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.active = false;
                state.last_error = Some(error.to_string());
                None
            }
        };
        inner.projects.insert(
            root.clone(),
            ProjectSync {
                status: status.clone(),
                watcher,
            },
        );
        snapshot(&status)
    }

    #[cfg(test)]
    fn status(&self, project: &Path) -> SyncSnapshot {
        let Ok(root) = project.canonicalize() else {
            return SyncSnapshot::degraded("project is unavailable".into());
        };
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .projects
            .get(&root)
            .map(|project| snapshot(&project.status))
            .unwrap_or_else(|| SyncSnapshot::degraded("project is not watched".into()))
    }
}

#[derive(Default)]
struct AutoSyncInner {
    projects: HashMap<PathBuf, ProjectSync>,
}

struct ProjectSync {
    status: Arc<Mutex<SyncState>>,
    watcher: Option<ProjectWatcher>,
}

struct ProjectWatcher {
    watcher: Option<RecommendedWatcher>,
    worker: Option<JoinHandle<()>>,
}

impl ProjectWatcher {
    fn start(
        root: PathBuf,
        debounce: Duration,
        status: Arc<Mutex<SyncState>>,
    ) -> notify::Result<Self> {
        fs::create_dir_all(root.join(".spectra")).map_err(notify::Error::io)?;
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;

        {
            let mut state = status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.active = true;
        }
        reconcile(&root, &status);

        let worker = thread::Builder::new()
            .name("spectra-autosync".into())
            .spawn({
                let root = root.clone();
                move || run_worker(root, debounce, status, receiver)
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher: Some(watcher),
            worker: Some(worker),
        })
    }
}

impl Drop for ProjectWatcher {
    fn drop(&mut self) {
        self.watcher.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SyncSnapshot {
    pub(crate) active: bool,
    pub(crate) pending: usize,
    pub(crate) sync_count: u64,
    pub(crate) last_report: Option<IndexReport>,
    pub(crate) last_error: Option<String>,
}

impl SyncSnapshot {
    fn degraded(error: String) -> Self {
        Self {
            last_error: Some(error),
            ..Self::default()
        }
    }

    pub(crate) fn compact(&self) -> String {
        let state = if self.active { "active" } else { "degraded" };
        let mut value = format!("autosync={state} pending={}", self.pending);
        if let Some(error) = &self.last_error {
            let error = error.split_whitespace().collect::<Vec<_>>().join(" ");
            let error = error.chars().take(96).collect::<String>();
            value.push_str(&format!(" error={error}"));
        }
        value
    }
}

#[derive(Default)]
struct SyncState {
    active: bool,
    pending: usize,
    sync_count: u64,
    last_report: Option<IndexReport>,
    last_error: Option<String>,
}

fn snapshot(status: &Arc<Mutex<SyncState>>) -> SyncSnapshot {
    let status = status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    SyncSnapshot {
        active: status.active,
        pending: status.pending,
        sync_count: status.sync_count,
        last_report: status.last_report.clone(),
        last_error: status.last_error.clone(),
    }
}

fn run_worker(
    root: PathBuf,
    debounce: Duration,
    status: Arc<Mutex<SyncState>>,
    receiver: Receiver<notify::Result<Event>>,
) {
    let mut pending = BTreeSet::new();
    loop {
        let event = if pending.is_empty() {
            match receiver.recv() {
                Ok(event) => event,
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(debounce) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if reconcile(&root, &status) {
                        pending.clear();
                        set_pending(&status, 0);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };

        match event {
            Ok(event) => {
                collect_paths(&root, &event, &mut pending);
                set_pending(&status, pending.len());
            }
            Err(error) => {
                let mut state = status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.active = false;
                state.last_error = Some(error.to_string());
            }
        }
    }
}

fn collect_paths(root: &Path, event: &Event, pending: &mut BTreeSet<PathBuf>) {
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    for path in &event.paths {
        if relevant_path(root, path, &event.kind) {
            pending.insert(path.clone());
        }
    }
}

fn relevant_path(root: &Path, path: &Path, kind: &EventKind) -> bool {
    let runtime = root.join(".spectra");
    if path.starts_with(&runtime) {
        return false;
    }
    if is_ignore_control(path) {
        return true;
    }
    if path.starts_with(root.join(".git")) {
        return false;
    }
    is_supported_path(path)
        || path.is_dir()
        || matches!(kind, EventKind::Remove(_)) && path.extension().is_none()
}

fn is_ignore_control(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".gitignore" | ".ignore" | "exclude")
    )
}

fn set_pending(status: &Arc<Mutex<SyncState>>, pending: usize) {
    status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pending = pending;
}

fn reconcile(root: &Path, status: &Arc<Mutex<SyncState>>) -> bool {
    match sync_project(root) {
        Ok((_, report)) => {
            if report.changed > 0 || report.removed > 0 {
                eprintln!(
                    "spectra: auto-synced {} changed and {} removed file(s)",
                    report.changed, report.removed
                );
            }
            let mut state = status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.sync_count += 1;
            state.last_report = Some(report);
            state.last_error = None;
            true
        }
        Err(error) => {
            let mut state = status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.last_error = Some(error.to_string());
            false
        }
    }
}

fn configured_debounce() -> Duration {
    let milliseconds = std::env::var("SPECTRA_WATCH_DEBOUNCE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| (MIN_DEBOUNCE_MS..=MAX_DEBOUNCE_MS).contains(value))
        .unwrap_or(DEFAULT_DEBOUNCE_MS);
    Duration::from_millis(milliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spectra_core::CodeIndex;
    use std::{fs, time::Instant};

    #[test]
    fn watcher_reconciles_changes_and_deletions() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        let source = root.join("src/lib.rs");
        fs::write(&source, "pub fn first() {}\n").unwrap();

        let autosync = AutoSync::with_debounce(Duration::from_millis(100));
        let initial = autosync.ensure_project(&root);
        assert!(initial.active, "{:?}", initial.last_error);
        assert_eq!(initial.sync_count, 1);

        fs::write(&source, "pub fn second() {}\n").unwrap();
        wait_for(|| autosync.status(&root).sync_count > initial.sync_count);
        let (changed, warm) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(warm.changed, 0);
        assert!(
            changed
                .graph
                .nodes
                .iter()
                .any(|node| changed.graph.atom(node.label) == "second")
        );

        let after_change = autosync.status(&root).sync_count;
        fs::remove_file(&source).unwrap();
        wait_for(|| autosync.status(&root).sync_count > after_change);
        let (_, warm) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(warm.removed, 0);

        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_and_unrelated_files_do_not_trigger_sync() {
        let root = Path::new("/project");
        assert!(!relevant_path(
            root,
            Path::new("/project/.spectra/index-v3.json"),
            &EventKind::Modify(notify::event::ModifyKind::Any)
        ));
        assert!(!relevant_path(
            root,
            Path::new("/project/README.md"),
            &EventKind::Modify(notify::event::ModifyKind::Any)
        ));
        assert!(relevant_path(
            root,
            Path::new("/project/src/lib.rs"),
            &EventKind::Modify(notify::event::ModifyKind::Any)
        ));
        assert!(relevant_path(
            root,
            Path::new("/project/.gitignore"),
            &EventKind::Modify(notify::event::ModifyKind::Any)
        ));
    }

    fn wait_for(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "watcher did not reconcile in time"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn temp_root() -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spectra-autosync-{}-{id}", std::process::id()))
    }
}
