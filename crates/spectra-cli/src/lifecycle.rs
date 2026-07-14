use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use spectra_core::{LedgerEventKind, LedgerSource, LedgerState, LedgerStore};

const MAX_INPUT_BYTES: usize = 1_048_576;
const MAX_PATHS: usize = 256;
const MAX_FIELD_CHARS: usize = 4_096;

#[derive(Debug, Deserialize)]
pub(crate) struct Envelope {
    version: u32,
    source: Source,
    cwd: PathBuf,
    event: LifecycleEvent,
}

#[derive(Debug, Deserialize)]
struct Source {
    harness: String,
    session_id: String,
    event_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LifecycleEvent {
    ContextRequested,
    AuthorizationRequested {
        action: String,
    },
    AuthorizationResult {
        allowed: bool,
        action: String,
    },
    EditObserved {
        paths: Vec<String>,
    },
    VerificationObserved {
        command: String,
        success: bool,
        exit_code: Option<i32>,
        output_bytes: usize,
    },
    TurnFinished {
        outcome: TurnOutcome,
        summary: String,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TurnOutcome {
    Completed,
    Blocked,
}

#[derive(Debug, Serialize)]
pub(crate) struct Response {
    version: u32,
    accepted: bool,
    duplicate: bool,
    sequence: u64,
    state: LedgerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<ContextProjection>,
}

#[derive(Debug, Serialize)]
struct ContextProjection {
    text: String,
    estimated_tokens: usize,
}

pub(crate) fn run_stdin() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(format!("lifecycle input exceeds {MAX_INPUT_BYTES} bytes").into());
    }
    let envelope: Envelope = serde_json::from_slice(&input)?;
    let response = ingest(envelope)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub(crate) fn ingest(envelope: Envelope) -> Result<Response, Box<dyn std::error::Error>> {
    validate(&envelope)?;
    let project = project_root(&envelope.cwd);
    let source = LedgerSource {
        harness: envelope.source.harness,
        session_id: envelope.source.session_id,
    };
    let event_id = format!(
        "{}:{}:{}",
        source.harness, source.session_id, envelope.source.event_id
    );
    LedgerStore::transaction(&project, |ledger| {
        let before = ledger.events().len();
        let mut context = None;
        let is_context = matches!(envelope.event, LifecycleEvent::ContextRequested);
        match envelope.event {
            LifecycleEvent::ContextRequested => {
                let projection = ledger.projection_for(&source);
                context = Some(ContextProjection {
                    text: projection.text,
                    estimated_tokens: projection.estimated_tokens,
                });
            }
            LifecycleEvent::AuthorizationRequested { action } => {
                ledger.append_once_for(
                    source.clone(),
                    event_id,
                    LedgerEventKind::AuthorizationRequested { action },
                )?;
            }
            LifecycleEvent::AuthorizationResult { allowed, action } => {
                let kind = if allowed {
                    LedgerEventKind::EditAuthorized { action }
                } else {
                    LedgerEventKind::Blocked {
                        reason: format!("authorization denied: {action}"),
                    }
                };
                ledger.append_once_for(source.clone(), event_id, kind)?;
            }
            LifecycleEvent::EditObserved { paths } => {
                ledger.append_once_for(
                    source.clone(),
                    event_id,
                    LedgerEventKind::EditObserved { paths },
                )?;
            }
            LifecycleEvent::VerificationObserved {
                command,
                success,
                exit_code,
                output_bytes,
            } => {
                ledger.append_once_for(
                    source.clone(),
                    format!("{event_id}:started"),
                    LedgerEventKind::VerificationStarted {
                        command: command.clone(),
                    },
                )?;
                ledger.append_once_for(
                    source.clone(),
                    format!("{event_id}:finished"),
                    LedgerEventKind::VerificationFinished {
                        command,
                        success,
                        exit_code,
                        output_bytes,
                    },
                )?;
            }
            LifecycleEvent::TurnFinished { outcome, summary } => {
                let kind = match outcome {
                    TurnOutcome::Completed => LedgerEventKind::Completed { summary },
                    TurnOutcome::Blocked => LedgerEventKind::Blocked { reason: summary },
                };
                ledger.append_once_for(source.clone(), event_id, kind)?;
            }
            LifecycleEvent::Blocked { reason } => {
                ledger.append_once_for(
                    source.clone(),
                    event_id,
                    LedgerEventKind::Blocked { reason },
                )?;
            }
        }
        let duplicate = ledger.events().len() == before && !is_context;
        let projection = ledger.projection_for(&source);
        Ok(Response {
            version: 1,
            accepted: true,
            duplicate,
            sequence: projection.sequence,
            state: projection.state,
            context,
        })
    })
    .map_err(Into::into)
}

fn validate(envelope: &Envelope) -> Result<(), Box<dyn std::error::Error>> {
    if envelope.version != 1 {
        return Err(format!(
            "unsupported lifecycle protocol version {}",
            envelope.version
        )
        .into());
    }
    if !envelope.cwd.is_absolute() || !envelope.cwd.is_dir() {
        return Err("cwd must be an existing absolute directory".into());
    }
    for (label, value, maximum) in [
        ("harness", envelope.source.harness.as_str(), 48_usize),
        ("session_id", envelope.source.session_id.as_str(), 96_usize),
        (
            "event_id",
            envelope.source.event_id.as_str(),
            MAX_FIELD_CHARS,
        ),
    ] {
        if value.trim().is_empty() || value.chars().count() > maximum {
            return Err(format!("{label} must contain 1..={maximum} characters").into());
        }
    }
    let check = |label: &str, value: &str| {
        if value.chars().count() > MAX_FIELD_CHARS {
            Err(format!("{label} exceeds {MAX_FIELD_CHARS} characters"))
        } else {
            Ok(())
        }
    };
    match &envelope.event {
        LifecycleEvent::AuthorizationRequested { action }
        | LifecycleEvent::AuthorizationResult { action, .. } => check("action", action)?,
        LifecycleEvent::EditObserved { paths } => {
            if paths.is_empty() || paths.len() > MAX_PATHS {
                return Err(format!("paths must contain 1..={MAX_PATHS} entries").into());
            }
            for path in paths {
                check("path", path)?;
            }
        }
        LifecycleEvent::VerificationObserved { command, .. } => check("command", command)?,
        LifecycleEvent::TurnFinished { summary, .. } => check("summary", summary)?,
        LifecycleEvent::Blocked { reason } => check("reason", reason)?,
        LifecycleEvent::ContextRequested => {}
    }
    Ok(())
}

fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}
