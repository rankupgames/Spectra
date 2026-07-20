use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use spectra_core::{EvidenceRecord, LedgerSource, PackedEvidence, pack_evidence};

const RECEIPTS_PATH: &str = ".spectra/context-receipts-v1.json";
const METRICS_PATH: &str = ".spectra/metrics-v1.json";
const LOCK_PATH: &str = ".spectra/context-runtime-v1.lock";
const MAX_SESSIONS: usize = 128;
const MAX_EVIDENCE: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Delivery {
    Delta,
    Full,
}

impl Delivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Delta => "delta",
            Self::Full => "full",
        }
    }
}

#[derive(Debug)]
pub(crate) struct DeliveryResult {
    pub packed: PackedEvidence,
    pub duplicate_evidence: usize,
    pub effective_delivery: Delivery,
}

pub(crate) struct DeliveryRequest<'a> {
    pub source: Option<&'a LedgerSource>,
    pub requested: Delivery,
    pub token_budget: usize,
    pub offset: usize,
    pub index_version: u32,
    pub ledger_sequence: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ReceiptFile {
    version: u32,
    salt: String,
    clock: u64,
    sessions: BTreeMap<String, ReceiptSession>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ReceiptSession {
    last_access: u64,
    index_version: u32,
    ledger_sequence: u64,
    evidence: BTreeMap<String, u64>,
}

pub(crate) fn deliver(
    project: &Path,
    records: Vec<EvidenceRecord>,
    request: DeliveryRequest<'_>,
) -> DeliveryResult {
    let Some(source) = request.source else {
        return DeliveryResult {
            packed: pack_evidence(records, request.token_budget, request.offset),
            duplicate_evidence: 0,
            effective_delivery: Delivery::Full,
        };
    };
    let fallback = records.clone();
    transaction(project, |receipts, recovered| {
        let effective = if recovered {
            Delivery::Full
        } else {
            request.requested
        };
        if receipts.salt.is_empty() {
            receipts.version = 1;
            receipts.salt = fresh_salt(project);
        }
        receipts.clock = receipts.clock.saturating_add(1);
        let clock = receipts.clock;
        let key = digest(
            &receipts.salt,
            &format!("{}\0{}", source.harness, source.session_id),
        );
        let session = receipts.sessions.entry(key).or_default();
        session.last_access = clock;
        session.index_version = request.index_version;
        session.ledger_sequence = request.ledger_sequence;
        if effective == Delivery::Full {
            session.evidence.clear();
        }
        let duplicate_evidence = if effective == Delivery::Delta {
            records
                .iter()
                .filter(|record| session.evidence.contains_key(&record.id))
                .count()
        } else {
            0
        };
        let candidates = records
            .into_iter()
            .filter(|record| {
                effective == Delivery::Full || !session.evidence.contains_key(&record.id)
            })
            .collect::<Vec<_>>();
        let effective_offset = if effective == Delivery::Delta {
            0
        } else {
            request.offset
        };
        let mut packed = pack_evidence(candidates, request.token_budget, effective_offset);
        for record in &packed.records {
            session.evidence.insert(record.id.clone(), clock);
        }
        trim_evidence(session);
        trim_sessions(receipts);
        if effective == Delivery::Delta && packed.next_offset.is_some() {
            packed.next_offset = Some(0);
        }
        DeliveryResult {
            packed,
            duplicate_evidence,
            effective_delivery: effective,
        }
    })
    .unwrap_or_else(|_| DeliveryResult {
        packed: pack_evidence(fallback, request.token_budget, request.offset),
        duplicate_evidence: 0,
        effective_delivery: Delivery::Full,
    })
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Metrics {
    pub version: u32,
    pub calls: u64,
    pub estimated_tokens_emitted: u64,
    pub duplicate_evidence_avoided: u64,
    pub maps_requested: u64,
    pub errors: u64,
    pub full_deliveries: u64,
    pub delta_deliveries: u64,
    pub latency_lt_10_ms: u64,
    pub latency_lt_100_ms: u64,
    pub latency_lt_1_s: u64,
    pub latency_gte_1_s: u64,
    pub calls_by_intent: BTreeMap<String, u64>,
}

pub(crate) struct MetricSample<'a> {
    pub intent: &'a str,
    pub estimated_tokens: usize,
    pub duplicates: usize,
    pub map: bool,
    pub error: bool,
    pub delivery: Delivery,
    pub elapsed: Duration,
}

pub(crate) fn record_metrics(project: &Path, sample: MetricSample<'_>) {
    if std::env::var("SPECTRA_METRICS")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("off"))
    {
        return;
    }
    let _ = with_lock(project, || {
        let path = project.join(METRICS_PATH);
        let mut metrics = read_json::<Metrics>(&path)
            .ok()
            .flatten()
            .unwrap_or_default();
        metrics.version = 1;
        metrics.calls += 1;
        metrics.estimated_tokens_emitted += sample.estimated_tokens as u64;
        metrics.duplicate_evidence_avoided += sample.duplicates as u64;
        metrics.maps_requested += u64::from(sample.map);
        metrics.errors += u64::from(sample.error);
        match sample.delivery {
            Delivery::Full => metrics.full_deliveries += 1,
            Delivery::Delta => metrics.delta_deliveries += 1,
        }
        *metrics
            .calls_by_intent
            .entry(sample.intent.to_owned())
            .or_default() += 1;
        match sample.elapsed.as_millis() {
            0..=9 => metrics.latency_lt_10_ms += 1,
            10..=99 => metrics.latency_lt_100_ms += 1,
            100..=999 => metrics.latency_lt_1_s += 1,
            _ => metrics.latency_gte_1_s += 1,
        }
        write_json(&path, &metrics)
    });
}

pub(crate) fn record_error(project: &Path) {
    if std::env::var("SPECTRA_METRICS")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("off"))
    {
        return;
    }
    let _ = with_lock(project, || {
        let path = project.join(METRICS_PATH);
        let mut metrics = read_json::<Metrics>(&path)
            .ok()
            .flatten()
            .unwrap_or_default();
        metrics.version = 1;
        metrics.errors += 1;
        write_json(&path, &metrics)
    });
}

pub(crate) fn read_metrics(project: &Path) -> Result<Metrics, Box<dyn std::error::Error>> {
    Ok(read_json(&project.join(METRICS_PATH))?.unwrap_or_default())
}

pub(crate) fn reset_metrics(project: &Path) -> Result<(), Box<dyn std::error::Error>> {
    with_lock(project, || {
        let path = project.join(METRICS_PATH);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    })
}

fn transaction<T>(
    project: &Path,
    operation: impl FnOnce(&mut ReceiptFile, bool) -> T,
) -> Result<T, Box<dyn std::error::Error>> {
    with_lock(project, || {
        let path = project.join(RECEIPTS_PATH);
        let (mut receipts, recovered) = match read_json::<ReceiptFile>(&path) {
            Ok(receipts) => (receipts.unwrap_or_default(), false),
            Err(_) => (ReceiptFile::default(), true),
        };
        let result = operation(&mut receipts, recovered);
        write_json(&path, &receipts)?;
        Ok(result)
    })
}

fn with_lock<T>(
    project: &Path,
    operation: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let path = project.join(LOCK_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut acquired = false;
    for _ in 0..1_000 {
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "{}", std::process::id())?;
                acquired = true;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                    .is_ok_and(|age| age > Duration::from_secs(30));
                if stale {
                    let _ = fs::remove_file(&path);
                } else {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !acquired {
        return Err("timed out acquiring context runtime lock".into());
    }
    let result = operation();
    let unlock = fs::remove_file(&path);
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error.into()),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    let mut file = AtomicWriteFile::open(path)?;
    file.write_all(&encoded)?;
    file.commit()?;
    Ok(())
}

fn trim_evidence(session: &mut ReceiptSession) {
    while session.evidence.len() > MAX_EVIDENCE {
        let oldest = session
            .evidence
            .iter()
            .min_by_key(|(id, clock)| (*clock, *id))
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            session.evidence.remove(&oldest);
        }
    }
}

fn trim_sessions(receipts: &mut ReceiptFile) {
    while receipts.sessions.len() > MAX_SESSIONS {
        let oldest = receipts
            .sessions
            .iter()
            .min_by_key(|(key, session)| (session.last_access, *key))
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            receipts.sessions.remove(&oldest);
        }
    }
}

fn fresh_salt(project: &Path) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    digest(
        "spectra-v1",
        &format!("{}\0{}\0{nanos}", project.display(), std::process::id()),
    )
}

pub(crate) fn digest(salt: &str, value: &str) -> String {
    fn hash(seed: u64, bytes: impl Iterator<Item = u8>) -> u64 {
        bytes.fold(seed, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
    }
    let bytes = salt.bytes().chain([0]).chain(value.bytes());
    let first = hash(0xcbf29ce484222325, bytes.clone());
    let second = hash(0x84222325cbf29ce4, bytes);
    format!("{first:016x}{second:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_project(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "spectra-context-state-{label}-{}",
            fresh_salt(Path::new(label))
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn evidence(value: &str) -> EvidenceRecord {
        EvidenceRecord {
            id: digest("evidence", value),
            priority: 1,
            text: value.into(),
        }
    }

    fn request(source: &LedgerSource, requested: Delivery) -> DeliveryRequest<'_> {
        DeliveryRequest {
            source: Some(source),
            requested,
            token_budget: 100,
            offset: 0,
            index_version: 4,
            ledger_sequence: 1,
        }
    }

    #[test]
    fn receipts_are_session_isolated_and_store_no_raw_session_or_body() {
        let project = temp_project("isolation");
        let first = LedgerSource {
            harness: "codex".into(),
            session_id: "raw-session-one".into(),
        };
        let second = LedgerSource {
            harness: "codex".into(),
            session_id: "raw-session-two".into(),
        };
        let records = vec![evidence("private source body")];
        let one = deliver(&project, records.clone(), request(&first, Delivery::Delta));
        assert_eq!(one.packed.records.len(), 1);
        let duplicate = deliver(&project, records.clone(), request(&first, Delivery::Delta));
        assert!(duplicate.packed.records.is_empty());
        assert_eq!(duplicate.duplicate_evidence, 1);
        let other = deliver(&project, records, request(&second, Delivery::Delta));
        assert_eq!(other.packed.records.len(), 1);
        let persisted = fs::read_to_string(project.join(RECEIPTS_PATH)).unwrap();
        assert!(!persisted.contains("raw-session"));
        assert!(!persisted.contains("private source body"));
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn full_delivery_resets_the_receipt_baseline() {
        let project = temp_project("full");
        let source = LedgerSource {
            harness: "custom".into(),
            session_id: "s1".into(),
        };
        let records = vec![evidence("anchor")];
        deliver(&project, records.clone(), request(&source, Delivery::Delta));
        let full = deliver(&project, records, request(&source, Delivery::Full));
        assert_eq!(full.packed.records.len(), 1);
        assert_eq!(full.effective_delivery, Delivery::Full);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn metrics_can_be_recorded_read_and_reset() {
        let project = temp_project("metrics");
        record_metrics(
            &project,
            MetricSample {
                intent: "locate",
                estimated_tokens: 42,
                duplicates: 3,
                map: false,
                error: false,
                delivery: Delivery::Delta,
                elapsed: Duration::from_millis(12),
            },
        );
        let metrics = read_metrics(&project).unwrap();
        assert_eq!(metrics.calls, 1);
        assert_eq!(metrics.duplicate_evidence_avoided, 3);
        assert_eq!(metrics.latency_lt_100_ms, 1);
        reset_metrics(&project).unwrap();
        assert_eq!(read_metrics(&project).unwrap().calls, 0);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn corrupt_receipts_fail_open_to_full_and_recover() {
        let project = temp_project("corrupt");
        let path = project.join(RECEIPTS_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not json").unwrap();
        let source = LedgerSource {
            harness: "custom".into(),
            session_id: "s1".into(),
        };
        let result = deliver(
            &project,
            vec![evidence("anchor")],
            request(&source, Delivery::Delta),
        );
        assert_eq!(result.effective_delivery, Delivery::Full);
        assert_eq!(result.packed.records.len(), 1);
        assert!(serde_json::from_slice::<ReceiptFile>(&fs::read(path).unwrap()).is_ok());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn concurrent_receipt_writers_preserve_each_delivery() {
        let project = temp_project("concurrent");
        let source = LedgerSource {
            harness: "custom".into(),
            session_id: "s1".into(),
        };
        let handles = (0..8)
            .map(|index| {
                let project = project.clone();
                let source = source.clone();
                std::thread::spawn(move || {
                    deliver(
                        &project,
                        vec![evidence(&format!("anchor-{index}"))],
                        request(&source, Delivery::Delta),
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert_eq!(handle.join().unwrap().packed.records.len(), 1);
        }
        let receipts: ReceiptFile =
            serde_json::from_slice(&fs::read(project.join(RECEIPTS_PATH)).unwrap()).unwrap();
        assert_eq!(receipts.sessions.values().next().unwrap().evidence.len(), 8);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn receipt_storage_enforces_session_and_evidence_caps() {
        let mut session = ReceiptSession::default();
        for index in 0..(MAX_EVIDENCE + 10) {
            session
                .evidence
                .insert(format!("e{index:03}"), index as u64);
        }
        trim_evidence(&mut session);
        assert_eq!(session.evidence.len(), MAX_EVIDENCE);
        assert!(!session.evidence.contains_key("e000"));

        let mut receipts = ReceiptFile::default();
        for index in 0..(MAX_SESSIONS + 10) {
            receipts.sessions.insert(
                format!("s{index:03}"),
                ReceiptSession {
                    last_access: index as u64,
                    ..ReceiptSession::default()
                },
            );
        }
        trim_sessions(&mut receipts);
        assert_eq!(receipts.sessions.len(), MAX_SESSIONS);
        assert!(!receipts.sessions.contains_key("s000"));
    }
}
