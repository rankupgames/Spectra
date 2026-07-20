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

use ignore::WalkBuilder;
#[cfg(not(target_os = "macos"))]
use notify::RecommendedWatcher as PlatformWatchers;
use notify::{Event, EventKind, RecursiveMode, Watcher};
#[cfg(target_os = "macos")]
use notify::{FsEventWatcher, PollWatcher};
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
    stop: mpsc::Sender<WatcherMessage>,
    worker: Option<JoinHandle<()>>,
}

enum WatcherMessage {
    Event(notify::Result<Event>),
    Stop,
}

#[derive(Default)]
struct WatchRegistrations {
    directories: BTreeSet<PathBuf>,
    #[cfg(target_os = "macos")]
    sources: BTreeSet<PathBuf>,
}

impl ProjectWatcher {
    fn start(
        root: PathBuf,
        debounce: Duration,
        status: Arc<Mutex<SyncState>>,
    ) -> notify::Result<Self> {
        fs::create_dir_all(root.join(".spectra")).map_err(notify::Error::io)?;
        let (sender, receiver) = mpsc::channel();
        let (watchers, watched) = platform_watchers(&root, debounce, &sender)?;

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
                move || run_worker(root, debounce, status, watchers, watched, receiver)
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            stop: sender,
            worker: Some(worker),
        })
    }
}

impl Drop for ProjectWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(WatcherMessage::Stop);
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
    mut watchers: PlatformWatchers,
    mut watched: WatchRegistrations,
    receiver: Receiver<WatcherMessage>,
) {
    let mut pending = BTreeSet::new();
    loop {
        let message = if pending.is_empty() {
            match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(debounce) {
                Ok(message) => message,
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

        match message {
            WatcherMessage::Stop => break,
            WatcherMessage::Event(Ok(event)) => {
                if watch_layout_changed(&event)
                    && let Err(error) = reconfigure_watches(&mut watchers, &root, &mut watched)
                {
                    let mut state = status
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.active = false;
                    state.last_error = Some(error.to_string());
                }
                collect_paths(&root, &event, &mut pending);
                set_pending(&status, pending.len());
            }
            WatcherMessage::Event(Err(error)) => {
                let mut state = status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.active = false;
                state.last_error = Some(error.to_string());
            }
        }
    }
}

#[cfg(target_os = "macos")]
struct PlatformWatchers {
    _native: Option<FsEventWatcher>,
    directory_poll: PollWatcher,
    source_poll: PollWatcher,
}

#[cfg(target_os = "macos")]
fn platform_watchers(
    root: &Path,
    debounce: Duration,
    sender: &mpsc::Sender<WatcherMessage>,
) -> notify::Result<(PlatformWatchers, WatchRegistrations)> {
    let native_sender = sender.clone();
    let native = FsEventWatcher::new(
        move |event| {
            let _ = native_sender.send(WatcherMessage::Event(event));
        },
        notify::Config::default(),
    )
    .and_then(|mut watcher| {
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(watcher)
    })
    .ok();

    let poll_interval = debounce.min(Duration::from_secs(1));
    let directory_sender = sender.clone();
    let directory_poll = PollWatcher::new(
        move |event| {
            let _ = directory_sender.send(WatcherMessage::Event(event));
        },
        notify::Config::default().with_poll_interval(poll_interval),
    )?;
    let source_sender = sender.clone();
    let source_poll = PollWatcher::new(
        move |event| {
            let _ = source_sender.send(WatcherMessage::Event(event));
        },
        notify::Config::default()
            .with_poll_interval(poll_interval)
            .with_compare_contents(true),
    )?;
    let mut watchers = PlatformWatchers {
        _native: native,
        directory_poll,
        source_poll,
    };
    let mut watched = WatchRegistrations::default();
    reconfigure_watches(&mut watchers, root, &mut watched)?;
    Ok((watchers, watched))
}

#[cfg(not(target_os = "macos"))]
fn platform_watchers(
    root: &Path,
    _debounce: Duration,
    sender: &mpsc::Sender<WatcherMessage>,
) -> notify::Result<(PlatformWatchers, WatchRegistrations)> {
    let event_sender = sender.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = event_sender.send(WatcherMessage::Event(event));
    })?;
    let mut watched = WatchRegistrations::default();
    reconfigure_watches(&mut watcher, root, &mut watched)?;
    Ok((watcher, watched))
}

#[cfg(target_os = "macos")]
fn reconfigure_watches(
    watchers: &mut PlatformWatchers,
    root: &Path,
    watched: &mut WatchRegistrations,
) -> notify::Result<()> {
    update_watches(
        &mut watchers.directory_poll,
        watch_directories(root),
        &mut watched.directories,
    )?;
    update_watches(
        &mut watchers.source_poll,
        watch_source_files(root),
        &mut watched.sources,
    )
}

#[cfg(not(target_os = "macos"))]
fn reconfigure_watches(
    watcher: &mut PlatformWatchers,
    root: &Path,
    watched: &mut WatchRegistrations,
) -> notify::Result<()> {
    update_watches(watcher, watch_directories(root), &mut watched.directories)
}

fn update_watches<W: Watcher>(
    watcher: &mut W,
    desired: BTreeSet<PathBuf>,
    watched: &mut BTreeSet<PathBuf>,
) -> notify::Result<()> {
    for path in watched.difference(&desired) {
        let _ = watcher.unwatch(path);
    }
    for path in desired.difference(watched) {
        watcher.watch(path, RecursiveMode::NonRecursive)?;
    }
    *watched = desired;
    Ok(())
}

fn watch_directories(root: &Path) -> BTreeSet<PathBuf> {
    let mut directories = BTreeSet::new();
    for entry in WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build()
    {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            directories.insert(entry.into_path());
        }
    }
    directories.insert(root.to_path_buf());
    directories
}

#[cfg(any(target_os = "macos", test))]
fn watch_source_files(root: &Path) -> BTreeSet<PathBuf> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| is_supported_path(path))
        .collect()
}

fn watch_layout_changed(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(_))
    ) || event.paths.iter().any(|path| is_ignore_control(path))
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
        wait_for(|| {
            let status = autosync.status(&root);
            status.sync_count >= 1 && status.pending == 0
        });
        let initial = autosync.status(&root);

        fs::write(&source, "pub fn second() {}\n").unwrap();
        wait_for(|| {
            let status = autosync.status(&root);
            status.sync_count > initial.sync_count && status.pending == 0
        });
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
        wait_for(|| {
            let status = autosync.status(&root);
            status.sync_count > after_change && status.pending == 0
        });
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
            Path::new("/project/.spectra/index-v4.json"),
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

    #[test]
    fn watch_registration_tracks_only_non_ignored_directories() {
        let root = temp_root();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        for index in 0..128 {
            fs::create_dir_all(root.join(format!("target/debug/deps/{index}"))).unwrap();
        }
        fs::write(root.join("target/generated.rs"), "pub fn ignored() {}\n").unwrap();
        fs::write(root.join(".gitignore"), "/target/\n").unwrap();

        let watched = watch_directories(&root);
        assert!(watched.contains(&root));
        assert!(watched.contains(&root.join("src")));
        assert!(watched.contains(&root.join("src/nested")));
        assert!(
            !watched
                .iter()
                .any(|path| path.starts_with(root.join("target")))
        );
        let sources = watch_source_files(&root);
        assert!(sources.contains(&root.join("src/lib.rs")));
        assert!(!sources.contains(&root.join("target/generated.rs")));

        fs::remove_file(root.join(".gitignore")).unwrap();
        let expanded = watch_directories(&root);
        assert!(expanded.contains(&root.join("target/debug/deps/127")));
        assert!(watch_source_files(&root).contains(&root.join("target/generated.rs")));
        fs::remove_dir_all(root).unwrap();
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
