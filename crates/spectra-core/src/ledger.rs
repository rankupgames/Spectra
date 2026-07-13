//! Append-only, replay-derived agent context state.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const LEDGER_VERSION: u32 = 1;
const LEDGER_PATH: &str = ".spectra/ledger-v1.jsonl";
const LEDGER_LOCK_PATH: &str = ".spectra/ledger-v1.lock";
const LOCK_ATTEMPTS: usize = 1_000;
const LOCK_RETRY: Duration = Duration::from_millis(2);
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerState {
    #[default]
    Idle,
    Observing,
    AwaitingAuthorization,
    Editing,
    Verifying,
    Blocked,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LedgerAnchor {
    pub visual_id: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LedgerEventKind {
    RepositorySynced {
        files: usize,
        changed: usize,
        removed: usize,
        nodes: usize,
        edges: usize,
    },
    MapRendered {
        map_id: String,
        query: String,
        anchors: Vec<LedgerAnchor>,
        nodes: usize,
        truncated: bool,
    },
    AuthorizationRequested {
        action: String,
    },
    EditAuthorized {
        action: String,
    },
    EditApplied {
        paths: Vec<String>,
    },
    VerificationStarted {
        command: String,
    },
    VerificationFinished {
        command: String,
        success: bool,
        exit_code: Option<i32>,
        output_bytes: usize,
    },
    Blocked {
        reason: String,
    },
    Completed {
        summary: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub version: u32,
    pub sequence: u64,
    pub recorded_at_unix_ms: u128,
    pub state_before: LedgerState,
    pub state_after: LedgerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub kind: LedgerEventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerProjection {
    pub state: LedgerState,
    pub sequence: u64,
    pub text: String,
    pub estimated_tokens: usize,
}

#[derive(Debug)]
pub struct LedgerStore {
    path: PathBuf,
    events: Vec<LedgerEvent>,
    state: LedgerState,
    recovered_truncated_tail: bool,
}

impl LedgerStore {
    /// Serializes a complete read/modify/append operation across hook processes.
    pub fn transaction<T>(
        project: &Path,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let _lock = LedgerLock::acquire(project)?;
        let mut store = Self::open(project)?;
        operation(&mut store)
    }

    /// Opens existing state or lazily creates it on the first append.
    pub fn open(project: &Path) -> Result<Self> {
        let path = project.join(LEDGER_PATH);
        if !path.exists() {
            return Ok(Self {
                path,
                events: Vec::new(),
                state: LedgerState::Idle,
                recovered_truncated_tail: false,
            });
        }
        let bytes = fs::read(&path)?;
        let mut events = Vec::new();
        let mut valid_bytes = 0_usize;
        let mut recovered = false;
        for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
            let complete = chunk.ends_with(b"\n");
            let record = if complete {
                &chunk[..chunk.len() - 1]
            } else {
                chunk
            };
            if record.is_empty() {
                valid_bytes += chunk.len();
                continue;
            }
            match serde_json::from_slice::<LedgerEvent>(record) {
                Ok(event) if complete => {
                    validate_replay_event(&events, &event)?;
                    events.push(event);
                    valid_bytes += chunk.len();
                }
                Ok(_) | Err(_) if !complete => {
                    recovered = true;
                    break;
                }
                Err(error) => {
                    return Err(Error::Ledger(format!(
                        "corrupt record at byte {valid_bytes}: {error}"
                    )));
                }
                Ok(_) => unreachable!(),
            }
        }
        if recovered {
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(valid_bytes as u64)?;
        }
        let state = events
            .last()
            .map(|event| event.state_after)
            .unwrap_or_default();
        Ok(Self {
            path,
            events,
            state,
            recovered_truncated_tail: recovered,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn state(&self) -> LedgerState {
        self.state
    }

    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }

    pub fn recovered_truncated_tail(&self) -> bool {
        self.recovered_truncated_tail
    }

    pub fn append(&mut self, kind: LedgerEventKind) -> Result<&LedgerEvent> {
        self.append_correlated(kind, None)
    }

    /// Appends once for a stable adapter event ID, making hook retries idempotent.
    pub fn append_once(
        &mut self,
        correlation_id: impl Into<String>,
        kind: LedgerEventKind,
    ) -> Result<&LedgerEvent> {
        let correlation_id = correlation_id.into();
        if let Some(index) = self
            .events
            .iter()
            .position(|event| event.correlation_id.as_deref() == Some(&correlation_id))
        {
            return Ok(&self.events[index]);
        }
        self.append_correlated(kind, Some(redact_text(&correlation_id)))
    }

    fn append_correlated(
        &mut self,
        kind: LedgerEventKind,
        correlation_id: Option<String>,
    ) -> Result<&LedgerEvent> {
        let state_after = transition(self.state, &kind)?;
        let event = LedgerEvent {
            version: LEDGER_VERSION,
            sequence: self.events.len() as u64 + 1,
            recorded_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| Error::Ledger(error.to_string()))?
                .as_millis(),
            state_before: self.state,
            state_after,
            correlation_id,
            kind: redact_event(kind),
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut encoded = serde_json::to_vec(&event)?;
        encoded.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&encoded)?;
        file.sync_data()?;
        self.state = state_after;
        self.events.push(event);
        Ok(self.events.last().expect("event was appended"))
    }

    pub fn projection(&self) -> LedgerProjection {
        let mut lines = vec![format!("S{} {:?}", self.events.len(), self.state)];
        for selector in [
            ProjectionSelector::Map,
            ProjectionSelector::Edit,
            ProjectionSelector::Verification,
            ProjectionSelector::Terminal,
        ] {
            if let Some(event) = self
                .events
                .iter()
                .rev()
                .find(|event| selector.matches(&event.kind))
            {
                lines.extend(project_event(&event.kind));
            }
        }
        let mut text = lines.join("\n");
        if text.chars().count() > 580 {
            text = text.chars().take(579).collect::<String>() + "…";
        }
        LedgerProjection {
            state: self.state,
            sequence: self.events.len() as u64,
            estimated_tokens: text.chars().count().div_ceil(4),
            text,
        }
    }
}

struct LedgerLock {
    path: PathBuf,
}

impl LedgerLock {
    fn acquire(project: &Path) -> Result<Self> {
        let path = project.join(LEDGER_LOCK_PATH);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        for _ in 0..LOCK_ATTEMPTS {
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if is_lock_contention(&error) => {
                    let stale = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                        .is_ok_and(|age| age > STALE_LOCK_AGE);
                    if stale {
                        if path.is_dir() {
                            let _ = fs::remove_dir(&path);
                        } else {
                            let _ = fs::remove_file(&path);
                        }
                    } else {
                        thread::sleep(LOCK_RETRY);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Ledger(
            "timed out waiting for the Ledger lock".into(),
        ))
    }
}

fn is_lock_contention(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return true;
    }

    // Windows can report a lock file that is being deleted or replaced as access denied,
    // sharing violation, or lock violation instead of already exists. All are transient here.
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(5 | 32 | 33))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn transition(state: LedgerState, event: &LedgerEventKind) -> Result<LedgerState> {
    use LedgerEventKind as Event;
    use LedgerState as State;
    let next = match event {
        Event::RepositorySynced { .. }
            if matches!(
                state,
                State::AwaitingAuthorization | State::Editing | State::Verifying
            ) =>
        {
            state
        }
        Event::RepositorySynced { .. } => State::Observing,
        Event::MapRendered { .. } if state == State::Observing => State::Idle,
        Event::MapRendered { .. } => state,
        Event::AuthorizationRequested { .. }
            if matches!(
                state,
                State::Idle | State::Observing | State::Blocked | State::Complete
            ) =>
        {
            State::AwaitingAuthorization
        }
        Event::EditAuthorized { .. } if state == State::AwaitingAuthorization => State::Editing,
        Event::EditApplied { .. } if state == State::Editing => State::Editing,
        Event::VerificationStarted { .. } if state == State::Editing => State::Verifying,
        Event::VerificationFinished { success: true, .. } if state == State::Verifying => {
            State::Complete
        }
        Event::VerificationFinished { success: false, .. } if state == State::Verifying => {
            State::Blocked
        }
        Event::Blocked { .. } => State::Blocked,
        Event::Completed { .. }
            if matches!(state, State::Editing | State::Verifying | State::Complete) =>
        {
            State::Complete
        }
        _ => {
            return Err(Error::Ledger(format!(
                "invalid transition from {state:?} via {}",
                event_name(event)
            )));
        }
    };
    Ok(next)
}

fn validate_replay_event(events: &[LedgerEvent], event: &LedgerEvent) -> Result<()> {
    let expected_sequence = events.len() as u64 + 1;
    let state = events
        .last()
        .map(|previous| previous.state_after)
        .unwrap_or_default();
    if event.version != LEDGER_VERSION
        || event.sequence != expected_sequence
        || event.state_before != state
        || transition(state, &event.kind)? != event.state_after
    {
        return Err(Error::Ledger(format!(
            "invalid replay record at sequence {}",
            event.sequence
        )));
    }
    Ok(())
}

fn redact_event(mut event: LedgerEventKind) -> LedgerEventKind {
    match &mut event {
        LedgerEventKind::MapRendered { query, .. }
        | LedgerEventKind::AuthorizationRequested { action: query }
        | LedgerEventKind::EditAuthorized { action: query }
        | LedgerEventKind::VerificationStarted { command: query }
        | LedgerEventKind::Blocked { reason: query }
        | LedgerEventKind::Completed { summary: query } => *query = redact_text(query),
        LedgerEventKind::VerificationFinished { command, .. } => {
            *command = redact_text(command);
        }
        LedgerEventKind::EditApplied { paths } => {
            for path in paths {
                *path = redact_text(path);
            }
        }
        LedgerEventKind::RepositorySynced { .. } => {}
    }
    event
}

pub fn redact_text(input: &str) -> String {
    input
        .lines()
        .take(12)
        .map(|line| {
            let upper = line.to_ascii_uppercase();
            if ["API_KEY=", "XAI_KEY=", "TOKEN=", "SECRET=", "PASSWORD="]
                .iter()
                .any(|marker| upper.contains(marker))
                || upper.contains("AUTHORIZATION: BEARER ")
            {
                "[REDACTED]".to_owned()
            } else {
                line.chars().take(160).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn project_event(event: &LedgerEventKind) -> Vec<String> {
    match event {
        LedgerEventKind::RepositorySynced {
            files,
            changed,
            removed,
            ..
        } => vec![format!(
            "sync files={files} changed={changed} removed={removed}"
        )],
        LedgerEventKind::MapRendered {
            map_id,
            anchors,
            nodes,
            truncated,
            ..
        } => {
            let anchor_text = anchors
                .iter()
                .take(3)
                .map(|anchor| format!("{}={}:{}", anchor.visual_id, anchor.path, anchor.start_line))
                .collect::<Vec<_>>()
                .join(" ");
            vec![
                format!("map {map_id} nodes={nodes} truncated={truncated}"),
                anchor_text,
            ]
        }
        LedgerEventKind::EditApplied { paths } => vec![format!("edit {}", paths.join(","))],
        LedgerEventKind::VerificationFinished {
            command,
            success,
            exit_code,
            output_bytes,
        } => vec![format!(
            "check {} success={} exit={:?} bytes={}",
            command, success, exit_code, output_bytes
        )],
        LedgerEventKind::Blocked { reason } => vec![format!("blocked {reason}")],
        LedgerEventKind::Completed { summary } => vec![format!("complete {summary}")],
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum ProjectionSelector {
    Map,
    Edit,
    Verification,
    Terminal,
}

impl ProjectionSelector {
    fn matches(self, event: &LedgerEventKind) -> bool {
        match self {
            Self::Map => matches!(event, LedgerEventKind::MapRendered { .. }),
            Self::Edit => matches!(event, LedgerEventKind::EditApplied { .. }),
            Self::Verification => matches!(event, LedgerEventKind::VerificationFinished { .. }),
            Self::Terminal => matches!(
                event,
                LedgerEventKind::Blocked { .. } | LedgerEventKind::Completed { .. }
            ),
        }
    }
}

fn event_name(event: &LedgerEventKind) -> &'static str {
    match event {
        LedgerEventKind::RepositorySynced { .. } => "repository_synced",
        LedgerEventKind::MapRendered { .. } => "map_rendered",
        LedgerEventKind::AuthorizationRequested { .. } => "authorization_requested",
        LedgerEventKind::EditAuthorized { .. } => "edit_authorized",
        LedgerEventKind::EditApplied { .. } => "edit_applied",
        LedgerEventKind::VerificationStarted { .. } => "verification_started",
        LedgerEventKind::VerificationFinished { .. } => "verification_finished",
        LedgerEventKind::Blocked { .. } => "blocked",
        LedgerEventKind::Completed { .. } => "completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_project(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spectra-ledger-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn append_replay_and_projection_are_deterministic() {
        let root = temp_project("replay");
        let mut store = LedgerStore::open(&root).unwrap();
        store
            .append(LedgerEventKind::RepositorySynced {
                files: 2,
                changed: 2,
                removed: 0,
                nodes: 4,
                edges: 3,
            })
            .unwrap();
        store
            .append(LedgerEventKind::MapRendered {
                map_id: "topology-1".into(),
                query: "where is launch".into(),
                anchors: vec![LedgerAnchor {
                    visual_id: "N1".into(),
                    path: "src/lib.rs".into(),
                    start_line: 4,
                    end_line: 9,
                }],
                nodes: 4,
                truncated: false,
            })
            .unwrap();
        let projection = store.projection();
        let replay = LedgerStore::open(&root).unwrap();
        assert_eq!(replay.state(), LedgerState::Idle);
        assert_eq!(replay.events(), store.events());
        assert_eq!(replay.projection(), projection);
        assert!(projection.estimated_tokens < 150);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_transition_is_not_appended() {
        let root = temp_project("invalid");
        let mut store = LedgerStore::open(&root).unwrap();
        assert!(
            store
                .append(LedgerEventKind::EditApplied {
                    paths: vec!["src/lib.rs".into()]
                })
                .is_err()
        );
        assert!(store.events().is_empty());
        assert!(!root.join(LEDGER_PATH).exists());
    }

    #[test]
    fn recovers_a_truncated_final_record() {
        let root = temp_project("truncated");
        let mut store = LedgerStore::open(&root).unwrap();
        store
            .append(LedgerEventKind::RepositorySynced {
                files: 1,
                changed: 1,
                removed: 0,
                nodes: 1,
                edges: 0,
            })
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(root.join(LEDGER_PATH))
            .unwrap();
        file.write_all(b"{\"version\":1").unwrap();
        drop(file);
        let replay = LedgerStore::open(&root).unwrap();
        assert!(replay.recovered_truncated_tail());
        assert_eq!(replay.events().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn redacts_credentials_before_persistence() {
        assert_eq!(redact_text("XAI_KEY=secret"), "[REDACTED]");
        assert_eq!(redact_text("Authorization: Bearer secret"), "[REDACTED]");
    }

    #[test]
    fn failed_verification_can_enter_an_authorized_repair_cycle() {
        let root = temp_project("repair");
        let mut store = LedgerStore::open(&root).unwrap();
        store
            .append(LedgerEventKind::AuthorizationRequested {
                action: "edit src/lib.rs".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::EditAuthorized {
                action: "edit src/lib.rs".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::EditApplied {
                paths: vec!["src/lib.rs".into()],
            })
            .unwrap();
        store
            .append(LedgerEventKind::VerificationStarted {
                command: "cargo test".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::VerificationFinished {
                command: "cargo test".into(),
                success: false,
                exit_code: Some(101),
                output_bytes: 4096,
            })
            .unwrap();
        store
            .append(LedgerEventKind::AuthorizationRequested {
                action: "repair src/lib.rs".into(),
            })
            .unwrap();
        assert_eq!(store.state(), LedgerState::AwaitingAuthorization);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projection_retains_map_edit_and_verification_state() {
        let root = temp_project("projection");
        let mut store = LedgerStore::open(&root).unwrap();
        store
            .append(LedgerEventKind::MapRendered {
                map_id: "topology-abc".into(),
                query: "find parser".into(),
                anchors: vec![LedgerAnchor {
                    visual_id: "N1".into(),
                    path: "src/parser.rs".into(),
                    start_line: 12,
                    end_line: 30,
                }],
                nodes: 20,
                truncated: false,
            })
            .unwrap();
        store
            .append(LedgerEventKind::AuthorizationRequested {
                action: "edit parser".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::EditAuthorized {
                action: "edit parser".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::EditApplied {
                paths: vec!["src/parser.rs".into()],
            })
            .unwrap();
        store
            .append(LedgerEventKind::VerificationStarted {
                command: "cargo test".into(),
            })
            .unwrap();
        store
            .append(LedgerEventKind::VerificationFinished {
                command: "cargo test".into(),
                success: true,
                exit_code: Some(0),
                output_bytes: 512,
            })
            .unwrap();
        let projection = store.projection();
        assert!(projection.text.contains("N1=src/parser.rs:12"));
        assert!(projection.text.contains("edit src/parser.rs"));
        assert!(projection.text.contains("check cargo test success=true"));
        assert!(projection.estimated_tokens < 150);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transactions_serialize_concurrent_hook_writers() {
        let root = Arc::new(temp_project("concurrent"));
        let writers: Vec<_> = (0..16)
            .map(|number| {
                let root = Arc::clone(&root);
                std::thread::spawn(move || {
                    LedgerStore::transaction(&root, |ledger| {
                        ledger.append_once(
                            format!("writer-{number}"),
                            LedgerEventKind::RepositorySynced {
                                files: 1,
                                changed: 1,
                                removed: 0,
                                nodes: number,
                                edges: 0,
                            },
                        )?;
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let replay = LedgerStore::open(&root).unwrap();
        assert_eq!(replay.events().len(), 16);
        assert_eq!(replay.events().last().unwrap().sequence, 16);
        assert_eq!(replay.state(), LedgerState::Observing);
        fs::remove_dir_all(root.as_ref()).unwrap();
    }

    #[test]
    fn lock_contention_classifies_platform_errors() {
        assert!(is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::AlreadyExists,
        )));
        assert!(!is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));

        #[cfg(windows)]
        for code in [5, 32, 33] {
            assert!(is_lock_contention(&std::io::Error::from_raw_os_error(code)));
        }

        #[cfg(not(windows))]
        assert!(!is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
    }

    #[test]
    fn correlated_appends_are_idempotent_and_backward_compatible() {
        let root = temp_project("correlated");
        LedgerStore::transaction(&root, |ledger| {
            let event = LedgerEventKind::RepositorySynced {
                files: 1,
                changed: 1,
                removed: 0,
                nodes: 1,
                edges: 0,
            };
            ledger.append_once("codex:event-1", event.clone())?;
            ledger.append_once("codex:event-1", event)?;
            Ok(())
        })
        .unwrap();
        let replay = LedgerStore::open(&root).unwrap();
        assert_eq!(replay.events().len(), 1);
        assert_eq!(
            replay.events()[0].correlation_id.as_deref(),
            Some("codex:event-1")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
