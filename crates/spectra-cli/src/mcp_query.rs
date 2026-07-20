use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use spectra_core::ledger::redact_text;
use spectra_core::{
    CodeIndex, EvidenceRecord, IndexReport, LedgerEventKind, LedgerSource, LedgerStore,
    SelectionOptions, estimate_tokens,
    graph::{EdgeId, NodeId},
    select_subgraph, supported_languages,
};

use crate::{
    autosync::{AutoSync, SyncSnapshot},
    context_state::{self, Delivery},
};

const MAX_TEXT: usize = 24_000;
const MAX_SOURCE_LINES: usize = 2_000;

pub(crate) struct ProjectView {
    pub(crate) root: PathBuf,
    pub(crate) index: CodeIndex,
    pub(crate) report: IndexReport,
    pub(crate) sync: SyncSnapshot,
}

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Callers,
    Callees,
}

#[derive(Clone, Copy)]
pub(crate) enum FileFormat {
    Tree,
    Flat,
    Grouped,
}

pub(crate) struct NodeViewOptions<'a> {
    pub(crate) symbol: Option<&'a str>,
    pub(crate) file: Option<&'a str>,
    pub(crate) line: Option<u32>,
    pub(crate) include_code: bool,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
    pub(crate) symbols_only: bool,
}

pub(crate) struct BriefOptions<'a> {
    pub(crate) query: &'a str,
    pub(crate) token_budget: usize,
    pub(crate) include_source: bool,
    pub(crate) source: Option<LedgerSource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextIntent {
    Auto,
    Resume,
    Locate,
    Flow,
    Change,
    Inspect,
}

impl ContextIntent {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Resume => "resume",
            Self::Locate => "locate",
            Self::Flow => "flow",
            Self::Change => "change",
            Self::Inspect => "inspect",
        }
    }
}

pub(crate) struct ContextOptions<'a> {
    pub(crate) query: &'a str,
    pub(crate) token_budget: usize,
    pub(crate) intent: ContextIntent,
    pub(crate) delivery: Delivery,
    pub(crate) source: Option<LedgerSource>,
    pub(crate) cursor: Option<&'a str>,
    pub(crate) map_requested: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextPacket {
    pub(crate) text: String,
}

pub(crate) struct ChangeOptions<'a> {
    pub(crate) base: &'a str,
    pub(crate) paths: Option<&'a [String]>,
    pub(crate) depth: usize,
    pub(crate) include_tests: bool,
    pub(crate) token_budget: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathMode {
    Execution,
    Dependency,
    Any,
}

pub(crate) struct PathOptions<'a> {
    pub(crate) from: &'a str,
    pub(crate) to: &'a str,
    pub(crate) from_file: Option<&'a str>,
    pub(crate) to_file: Option<&'a str>,
    pub(crate) mode: PathMode,
    pub(crate) max_hops: usize,
}

pub(crate) fn open_project(
    autosync: &AutoSync,
    project_path: Option<&str>,
) -> Result<ProjectView, Box<dyn std::error::Error>> {
    if let Some(path) = project_path
        && path.len() > 4_096
    {
        return Err("projectPath exceeds 4096 characters".into());
    }
    let root = project_path
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()).into());
    }
    let sync = autosync.ensure_project(&root);
    let (index, report) = CodeIndex::refresh(&root)?;
    Ok(ProjectView {
        root,
        index,
        report,
        sync,
    })
}

pub(crate) fn context_packet(
    view: &ProjectView,
    options: ContextOptions<'_>,
) -> Result<ContextPacket, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let budget = options.token_budget.clamp(128, 2_000);
    let intent = if options.intent == ContextIntent::Auto {
        classify_intent(options.query)
    } else {
        options.intent
    };
    let cursor = options
        .cursor
        .map(decode_cursor)
        .transpose()?
        .unwrap_or_default();
    let query_hash = context_state::digest("context-query-v1", options.query);
    if options.cursor.is_some()
        && (cursor.query_hash != query_hash
            || cursor.index_version != view.index.version
            || cursor.intent != intent.as_str())
    {
        return Err("cursor_stale: query, intent, or index changed; restart without cursor".into());
    }

    let selection = select_subgraph(
        &view.index,
        options.query,
        SelectionOptions { max_nodes: 48 },
    );
    let ledger = LedgerStore::open(&view.root)?;
    let projection = options
        .source
        .as_ref()
        .map(|source| ledger.projection_for(source).text)
        .unwrap_or_else(|| ledger.project_facts().text);
    let sequence = ledger.events().len() as u64;
    let mut records = Vec::new();
    if !projection.trim().is_empty() {
        push_evidence(
            &mut records,
            1_000,
            format!("state {}", projection.replace('\n', "; ")),
        );
    }

    let anchors = selection
        .nodes
        .iter()
        .copied()
        .filter(|node| view.index.graph.kind(*node) != "file")
        .take(12)
        .collect::<Vec<_>>();
    let anchor_ids = anchors
        .iter()
        .enumerate()
        .map(|(index, node)| (*node, format!("A{}", index + 1)))
        .collect::<BTreeMap<_, _>>();
    for node in &anchors {
        let id = &anchor_ids[node];
        let span = view.index.spans.get(node);
        let path = node_path(view, *node).unwrap_or("<unknown>");
        let qualified = view
            .index
            .qualified_names
            .get(node)
            .map(String::as_str)
            .unwrap_or_else(|| view.index.graph.label(*node));
        push_evidence(
            &mut records,
            900,
            format!(
                "{id} {} {qualified} @ {path}:{}-{}",
                view.index.graph.kind(*node),
                span.map(|span| span.start_line).unwrap_or(0),
                span.map(|span| span.end_line).unwrap_or(0)
            ),
        );
    }

    if intent == ContextIntent::Change {
        for line in changes(
            view,
            ChangeOptions {
                base: "HEAD",
                paths: None,
                depth: 2,
                include_tests: true,
                token_budget: 2_000,
            },
        )
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        {
            push_evidence(&mut records, 850, format!("change {line}"));
        }
    }

    if matches!(
        intent,
        ContextIntent::Flow | ContextIntent::Locate | ContextIntent::Inspect
    ) {
        for edge in &view.index.graph.edges {
            let (Some(source), Some(target)) =
                (anchor_ids.get(&edge.source), anchor_ids.get(&edge.target))
            else {
                continue;
            };
            let kind = view.index.graph.atom(edge.kind);
            if kind != "contains" {
                push_evidence(&mut records, 800, format!("E {source} {kind} {target}"));
            }
        }
    }

    if intent == ContextIntent::Inspect {
        for node in anchors.iter().take(3) {
            let Some(path) = node_path(view, *node) else {
                continue;
            };
            let source = source_window(view, path, &[*node], 24, MAX_TEXT)
                .unwrap_or_else(|error| format!("> Source unavailable: {error}"));
            for line in source.lines() {
                push_evidence(&mut records, 600, format!("S {} {line}", anchor_ids[node]));
            }
        }
    }
    if let Some(first) = anchors.first() {
        push_evidence(
            &mut records,
            100,
            format!(
                "next inspect {} file={}",
                view.index.graph.label(*first),
                node_path(view, *first).unwrap_or("<unknown>")
            ),
        );
    }

    let delivery = context_state::deliver(
        &view.root,
        records,
        context_state::DeliveryRequest {
            source: options.source.as_ref(),
            requested: options.delivery,
            token_budget: budget.saturating_sub(64),
            offset: cursor.offset,
            index_version: view.index.version,
            ledger_sequence: sequence,
        },
    );
    let packet_seed = delivery
        .packed
        .records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<Vec<_>>()
        .join(":");
    let packet_id = &context_state::digest("context-packet-v1", &packet_seed)[..12];
    let mut lines = vec![String::new()];
    lines.extend(
        delivery
            .packed
            .records
            .iter()
            .map(|record| record.text.clone()),
    );
    if delivery.packed.records.is_empty() && delivery.duplicate_evidence > 0 {
        lines.push(format!("unchanged sequence={sequence}"));
    }
    let next = delivery.packed.next_offset.map(|offset| {
        encode_cursor(&ContextCursor {
            query_hash: query_hash.clone(),
            index_version: view.index.version,
            intent: intent.as_str().into(),
            offset,
        })
    });
    lines.push(format!(
        "omitted={}{}",
        delivery.packed.omitted,
        next.as_ref()
            .map(|cursor| format!(" next={cursor}"))
            .unwrap_or_default()
    ));
    let provisional = lines.join("\n");
    let used = estimate_tokens(&provisional);
    lines[0] = format!(
        "C1 id=p{packet_id} intent={} index=v{} budget={budget} used≈{used} delivery={} image_cost={}",
        intent.as_str(),
        view.index.version,
        delivery.effective_delivery.as_str(),
        if options.map_requested {
            "provider"
        } else {
            "none"
        }
    );
    let mut text = lines.join("\n");
    let mut estimated_tokens = estimate_tokens(&text);
    if estimated_tokens != used {
        lines[0] = lines[0].replace(&format!("used≈{used}"), &format!("used≈{estimated_tokens}"));
        text = lines.join("\n");
        estimated_tokens = estimate_tokens(&text);
    }
    context_state::record_metrics(
        &view.root,
        context_state::MetricSample {
            intent: intent.as_str(),
            estimated_tokens,
            duplicates: delivery.duplicate_evidence,
            map: options.map_requested,
            error: false,
            delivery: delivery.effective_delivery,
            elapsed: started.elapsed(),
        },
    );
    Ok(ContextPacket { text })
}

fn classify_intent(query: &str) -> ContextIntent {
    let query = query.to_ascii_lowercase();
    if [
        "resume", "continue", "pick up", "blocked", "failed", "failure",
    ]
    .iter()
    .any(|term| query.contains(term))
    {
        ContextIntent::Resume
    } else if [
        "change", "changed", "impact", "worktree", "affected", "tests",
    ]
    .iter()
    .any(|term| query.contains(term))
    {
        ContextIntent::Change
    } else if query.starts_with("how ")
        || [" flow ", " reach ", " path ", " calls "]
            .iter()
            .any(|term| query.contains(term))
    {
        ContextIntent::Flow
    } else if ["source", "implementation", "inspect", "show code", "read "]
        .iter()
        .any(|term| query.contains(term))
    {
        ContextIntent::Inspect
    } else {
        ContextIntent::Locate
    }
}

fn push_evidence(records: &mut Vec<EvidenceRecord>, priority: i32, text: String) {
    records.push(EvidenceRecord {
        id: context_state::digest("context-evidence-v1", &text),
        priority,
        text,
    });
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ContextCursor {
    query_hash: String,
    index_version: u32,
    intent: String,
    offset: usize,
}

fn encode_cursor(cursor: &ContextCursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).unwrap_or_default())
}

fn decode_cursor(value: &str) -> Result<ContextCursor, Box<dyn std::error::Error>> {
    if value.len() > 2_048 {
        return Err("cursor exceeds 2048 characters".into());
    }
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(value)?)?)
}

pub(crate) fn search(view: &ProjectView, query: &str, kind: Option<&str>, limit: usize) -> String {
    let query = query.trim().to_ascii_lowercase();
    let kind = kind.map(|kind| if kind == "type" { "type_alias" } else { kind });
    let mut matches = view
        .index
        .graph
        .nodes
        .iter()
        .filter(|node| view.index.graph.kind(node.id) != "file")
        .filter(|node| kind.is_none_or(|kind| view.index.graph.kind(node.id) == kind))
        .filter_map(|node| {
            let label = view.index.graph.label(node.id);
            let qualified = view
                .index
                .qualified_names
                .get(&node.id)
                .map(String::as_str)
                .unwrap_or(label);
            let path = node_path(view, node.id).unwrap_or("");
            let label_lower = label.to_ascii_lowercase();
            let qualified_lower = qualified.to_ascii_lowercase();
            let path_lower = path.to_ascii_lowercase();
            let score = if label_lower == query {
                0
            } else if label_lower.starts_with(&query) {
                1
            } else if label_lower.contains(&query) {
                2
            } else if qualified_lower.contains(&query) {
                3
            } else if path_lower.contains(&query) {
                4
            } else {
                return None;
            };
            Some((score, path.to_owned(), node.id))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.truncate(limit.clamp(1, 100));
    if matches.is_empty() {
        return format!("No results found for \"{query}\"");
    }
    let mut lines = vec![
        format!("**Search results for {query} ({})**", matches.len()),
        String::new(),
    ];
    for (_, _, node) in matches {
        lines.push(format_node(view, node, None));
    }
    bounded(lines.join("\n"), MAX_TEXT)
}

pub(crate) fn relationships(
    view: &ProjectView,
    symbol: &str,
    file: Option<&str>,
    direction: Direction,
    limit: usize,
) -> String {
    let roots = symbol_nodes(view, symbol, file);
    let heading = match direction {
        Direction::Callers => "Callers of",
        Direction::Callees => "Callees of",
    };
    if roots.is_empty() {
        return format!("Symbol \"{symbol}\" not found in the codebase");
    }
    let mut related = BTreeMap::new();
    for root in roots {
        let edges = match direction {
            Direction::Callers => view.index.graph.incoming(root),
            Direction::Callees => view.index.graph.outgoing(root),
        };
        for edge_id in edges {
            let Some(edge) = view.index.graph.edge(*edge_id) else {
                continue;
            };
            let edge_kind = view.index.graph.atom(edge.kind);
            if !is_execution_edge(edge_kind) {
                continue;
            }
            let node = match direction {
                Direction::Callers => edge.source,
                Direction::Callees => edge.target,
            };
            related.entry(node).or_insert_with(|| edge_kind.to_owned());
        }
    }
    let mut lines = vec![format!("**{heading} {symbol}**"), String::new()];
    if related.is_empty() {
        lines.push(format!("No {} found.", heading.to_ascii_lowercase()));
    } else {
        for (node, edge) in related.into_iter().take(limit.clamp(1, 100)) {
            lines.push(format_node(view, node, Some(&edge)));
        }
    }
    bounded(lines.join("\n"), MAX_TEXT)
}

pub(crate) fn impact(view: &ProjectView, symbol: &str, file: Option<&str>, depth: usize) -> String {
    let roots = symbol_nodes(view, symbol, file);
    if roots.is_empty() {
        return format!("Symbol \"{symbol}\" not found in the codebase");
    }
    let root_set = roots.iter().copied().collect::<BTreeSet<_>>();
    let mut seen = root_set.clone();
    let mut queue = roots
        .iter()
        .copied()
        .map(|node| (node, 0_usize))
        .collect::<VecDeque<_>>();
    let mut levels: BTreeMap<usize, BTreeSet<NodeId>> = BTreeMap::new();
    while let Some((node, level)) = queue.pop_front() {
        if level >= depth.clamp(1, 10) {
            continue;
        }
        for edge_id in view.index.graph.incoming(node) {
            let Some(edge) = view.index.graph.edge(*edge_id) else {
                continue;
            };
            if view.index.graph.atom(edge.kind) == "contains" || !seen.insert(edge.source) {
                continue;
            }
            levels.entry(level + 1).or_default().insert(edge.source);
            queue.push_back((edge.source, level + 1));
        }
    }
    let affected = seen.len().saturating_sub(root_set.len());
    let mut lines = vec![
        format!("**Impact of {symbol}** — {affected} affected symbols"),
        String::new(),
    ];
    for (level, nodes) in levels {
        lines.push(format!("**Depth {level}**"));
        for node in nodes {
            lines.push(format_node(view, node, None));
        }
        lines.push(String::new());
    }
    bounded(lines.join("\n"), MAX_TEXT)
}

pub(crate) fn explore(view: &ProjectView, query: &str, max_files: usize) -> String {
    explore_budgeted(view, query, max_files, MAX_TEXT)
}

pub(crate) fn explore_budgeted(
    view: &ProjectView,
    query: &str,
    max_files: usize,
    max_chars: usize,
) -> String {
    let selection = select_subgraph(&view.index, query, SelectionOptions { max_nodes: 96 });
    let selected = selection.nodes.iter().copied().collect::<BTreeSet<_>>();
    let mut by_file: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for node in &selection.nodes {
        if view.index.graph.kind(*node) == "boundary" {
            continue;
        }
        if let Some(path) = node_path(view, *node) {
            by_file.entry(path.to_owned()).or_default().push(*node);
        }
    }
    let mut ranked = by_file.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, nodes)| {
        nodes
            .iter()
            .filter_map(|node| selection.distances.get(node))
            .min()
            .copied()
            .unwrap_or(u32::MAX)
    });
    ranked.truncate(max_files.clamp(1, 20));

    let mut sections = vec![format!("# Spectra exploration: {query}"), String::new()];
    for (path, nodes) in ranked {
        let symbols = nodes
            .iter()
            .filter(|node| view.index.graph.kind(**node) != "file")
            .map(|node| {
                format!(
                    "{}({})",
                    view.index.graph.label(*node),
                    view.index.graph.kind(*node)
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(8)
            .collect::<Vec<_>>()
            .join(", ");
        sections.push(format!("**`{path}`** — {symbols}"));
        match source_window(view, &path, &nodes, 220, 7_000) {
            Ok(source) => sections.push(source),
            Err(error) => sections.push(format!("> Source unavailable: {error}")),
        }
        sections.push(String::new());
    }
    let mut edges = Vec::new();
    for edge in &view.index.graph.edges {
        if selected.contains(&edge.source)
            && selected.contains(&edge.target)
            && view.index.graph.atom(edge.kind) != "contains"
        {
            edges.push(format!(
                "- {} —{}→ {}",
                view.index.graph.label(edge.source),
                view.index.graph.atom(edge.kind),
                view.index.graph.label(edge.target)
            ));
        }
    }
    if !edges.is_empty() {
        sections.push("**Relationships**".into());
        sections.extend(edges.into_iter().take(40));
    }
    sections.push(String::new());
    sections.push(format!(
        "nodes={} truncated={} index=v{} · {}",
        selection.nodes.len(),
        selection.truncated,
        view.index.version,
        view.sync.compact()
    ));
    bounded(sections.join("\n"), max_chars.clamp(512, MAX_TEXT))
}

pub(crate) fn brief(view: &ProjectView, options: BriefOptions<'_>) -> String {
    let token_budget = options.token_budget.clamp(128, 2_000);
    let max_chars = token_budget * 4;
    let selection = select_subgraph(
        &view.index,
        options.query,
        SelectionOptions { max_nodes: 48 },
    );
    let continuity = match LedgerStore::open(&view.root) {
        Ok(ledger) => options
            .source
            .as_ref()
            .map(|source| ledger.projection_for(source).text)
            .unwrap_or_else(|| ledger.project_facts().text),
        Err(error) => format!("continuity unavailable: {error}"),
    };
    let mut sections = vec![
        "# Spectra brief".into(),
        format!("goal={}", redact_text(options.query)),
        format!("sync={}", view.sync.compact()),
        String::new(),
        "## Continuity".into(),
        continuity,
        String::new(),
        "## Ranked anchors".into(),
    ];
    let anchors = selection
        .nodes
        .iter()
        .copied()
        .filter(|node| view.index.graph.kind(*node) != "file")
        .take(12)
        .collect::<Vec<_>>();
    if anchors.is_empty() {
        sections.push("No matching indexed anchors.".into());
    } else {
        sections.extend(anchors.iter().map(|node| format_node(view, *node, None)));
    }
    let boundaries = anchors
        .iter()
        .filter(|node| view.index.graph.kind(**node) == "boundary")
        .map(|node| view.index.graph.label(*node))
        .collect::<BTreeSet<_>>();
    if !boundaries.is_empty() {
        sections.push(String::new());
        sections.push(format!(
            "uncertain_boundaries={}",
            boundaries.into_iter().collect::<Vec<_>>().join(",")
        ));
    }
    if options.include_source {
        let mut by_file: BTreeMap<&str, Vec<NodeId>> = BTreeMap::new();
        for node in &anchors {
            if let Some(path) = node_path(view, *node) {
                by_file.entry(path).or_default().push(*node);
            }
        }
        if !by_file.is_empty() {
            sections.push(String::new());
            sections.push("## Bounded source".into());
            for (path, nodes) in by_file.into_iter().take(3) {
                sections.push(format!("**`{path}`**"));
                match source_window(view, path, &nodes, 24, 2_400) {
                    Ok(source) => sections.push(redact_text(&source)),
                    Err(error) => sections.push(format!("> Source unavailable: {error}")),
                }
            }
        }
    }
    sections.push(String::new());
    sections.push(if let Some(node) = anchors.first() {
        format!(
            "next=spectra_node symbol={} file={}",
            view.index.graph.label(*node),
            node_path(view, *node).unwrap_or("<unknown>")
        )
    } else {
        "next=refine the brief query".into()
    });
    bounded(sections.join("\n"), max_chars)
}

pub(crate) fn changes(view: &ProjectView, options: ChangeOptions<'_>) -> String {
    let token_budget = options.token_budget.clamp(128, 2_000);
    let discovered = discover_changes(view, options.base, options.paths);
    let mut direct = BTreeSet::new();
    for (path, ranges) in &discovered.paths {
        for node in nodes_in_file(view, path) {
            let overlaps = ranges.is_empty()
                || view.index.spans.get(&node).is_some_and(|span| {
                    ranges
                        .iter()
                        .any(|(start, end)| span.start_line <= *end && span.end_line >= *start)
                });
            if overlaps {
                direct.insert(node);
            }
        }
    }
    let impact_ranks = incoming_impact(view, &direct, options.depth.clamp(1, 10));
    let impact = impact_ranks.keys().copied().collect::<BTreeSet<_>>();
    let mut tests = direct
        .iter()
        .chain(impact.iter())
        .copied()
        .filter(|node| node_path(view, *node).is_some_and(is_test_path))
        .map(|node| (impact_ranks.get(&node).copied().unwrap_or(0), node))
        .collect::<Vec<_>>();
    tests.sort_by_key(|(distance, node)| {
        (
            *distance,
            node_path(view, *node).unwrap_or(""),
            view.index.graph.label(*node),
        )
    });
    let mut sections = vec![
        "# Spectra changes".into(),
        format!("provenance={}", discovered.provenance),
        format!(
            "files={} direct_symbols={} affected_symbols={}",
            discovered.paths.len(),
            direct.len(),
            impact.len()
        ),
        String::new(),
        "## Changed files".into(),
    ];
    if discovered.paths.is_empty() {
        sections.push("No changed paths discovered.".into());
    } else {
        for (path, ranges) in &discovered.paths {
            let suffix = if discovered.deleted.contains(path) {
                " deleted".to_owned()
            } else if ranges.is_empty() {
                String::new()
            } else {
                format!(
                    " lines={}",
                    ranges
                        .iter()
                        .map(|(start, end)| format!("{start}-{end}"))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            sections.push(format!("- {path}{suffix}"));
        }
    }
    append_node_section(view, &mut sections, "Directly changed symbols", &direct, 24);
    append_node_section(view, &mut sections, "Affected symbols", &impact, 32);
    if options.include_tests && !tests.is_empty() {
        sections.push(String::new());
        sections.push("## Ranked tests".into());
        sections.extend(
            tests
                .into_iter()
                .take(24)
                .enumerate()
                .map(|(rank, (distance, node))| {
                    format!(
                        "{}. distance={distance} {}",
                        rank + 1,
                        format_node(view, node, None).trim_start_matches("- ")
                    )
                }),
        );
    }
    bounded(sections.join("\n"), token_budget * 4)
}

pub(crate) fn typed_paths(view: &ProjectView, options: PathOptions<'_>) -> String {
    let from = symbol_nodes(view, options.from, options.from_file);
    let to = symbol_nodes(view, options.to, options.to_file);
    if from.len() != 1 {
        return endpoint_candidates(view, "from", options.from, &from);
    }
    if to.len() != 1 {
        return endpoint_candidates(view, "to", options.to, &to);
    }
    let start = from[0];
    let target = to[0];
    let paths = shortest_paths(
        view,
        start,
        target,
        options.mode,
        options.max_hops.clamp(1, 20),
        3,
    );
    if paths.is_empty() {
        let reverse = shortest_paths(
            view,
            target,
            start,
            options.mode,
            options.max_hops.clamp(1, 20),
            1,
        );
        return if reverse.is_empty() {
            format!(
                "No directed {:?} path found from {} to {} within {} hops.",
                options.mode, options.from, options.to, options.max_hops
            )
        } else {
            format!(
                "No directed {:?} path found from {} to {}; a reverse path exists from {} to {}.",
                options.mode, options.from, options.to, options.to, options.from
            )
        };
    }
    let mut sections = vec![format!(
        "# Spectra paths: {} → {} ({:?})",
        options.from, options.to, options.mode
    )];
    for (index, path) in paths.iter().enumerate() {
        sections.push(String::new());
        sections.push(format!("## Path {} ({} hops)", index + 1, path.len()));
        sections.push(format_node(view, start, None));
        let mut current = start;
        for edge_id in path {
            let Some(edge) = view.index.graph.edge(*edge_id) else {
                continue;
            };
            debug_assert_eq!(edge.source, current);
            sections.push(format!(
                "  -{}→ {}",
                view.index.graph.atom(edge.kind),
                format_node(view, edge.target, None).trim_start_matches("- ")
            ));
            current = edge.target;
        }
    }
    bounded(sections.join("\n"), MAX_TEXT)
}

pub(crate) fn node_view(view: &ProjectView, options: NodeViewOptions<'_>) -> String {
    if options.symbol.is_none() {
        let Some(file) = options.file else {
            return "Pass `symbol`, or pass `file` alone to inspect an indexed file.".into();
        };
        return file_view(
            view,
            file,
            options.offset,
            options.limit,
            options.symbols_only,
        );
    }
    let symbol = options.symbol.unwrap_or_default();
    let mut nodes = symbol_nodes(view, symbol, options.file);
    if let Some(line) = options.line {
        nodes.sort_by_key(|node| {
            view.index
                .spans
                .get(node)
                .map(|span| span.start_line.abs_diff(line))
                .unwrap_or(u32::MAX)
        });
        nodes.truncate(1);
    }
    if nodes.is_empty() {
        return format!("Symbol \"{symbol}\" not found in the codebase");
    }
    let mut sections = Vec::new();
    for node in nodes.into_iter().take(12) {
        let path = node_path(view, node).unwrap_or("<unknown>");
        let span = view.index.spans.get(&node);
        sections.push(format!(
            "**{}** ({}) — {}:{}-{}",
            view.index
                .qualified_names
                .get(&node)
                .map(String::as_str)
                .unwrap_or_else(|| view.index.graph.label(node)),
            view.index.graph.kind(node),
            path,
            span.map(|span| span.start_line).unwrap_or(0),
            span.map(|span| span.end_line).unwrap_or(0)
        ));
        if options.include_code {
            match source_window(view, path, &[node], 240, 12_000) {
                Ok(source) => sections.push(source),
                Err(error) => sections.push(format!("> Source unavailable: {error}")),
            }
        }
        let trail = direct_trail(view, node);
        if !trail.is_empty() {
            sections.push(trail);
        }
        sections.push(String::new());
    }
    bounded(sections.join("\n"), MAX_TEXT)
}

pub(crate) fn status(view: &ProjectView) -> String {
    let mut kinds = BTreeMap::new();
    for node in &view.index.graph.nodes {
        *kinds
            .entry(view.index.graph.kind(node.id))
            .or_insert(0_usize) += 1;
    }
    let files = indexed_files(view);
    let mut languages = BTreeMap::new();
    for file in &files {
        *languages.entry(file.language.as_str()).or_insert(0_usize) += 1;
    }
    let mut lines = vec![
        "**Spectra Status**".into(),
        String::new(),
        format!("**Files indexed:** {}", view.report.files),
        format!("**Total nodes:** {}", view.report.nodes),
        format!("**Total edges:** {}", view.report.edges),
        format!("**Index version:** {}", view.index.version),
        format!("**Auto-sync:** {}", view.sync.compact()),
        String::new(),
        "**Nodes by Kind:**".into(),
    ];
    lines.extend(
        kinds
            .into_iter()
            .map(|(kind, count)| format!("- {kind}: {count}")),
    );
    lines.push(String::new());
    lines.push("**Languages:**".into());
    lines.extend(
        languages
            .into_iter()
            .map(|(language, count)| format!("- {language}: {count}")),
    );
    bounded(lines.join("\n"), MAX_TEXT)
}

pub(crate) fn files(
    view: &ProjectView,
    path: Option<&str>,
    pattern: Option<&str>,
    format: FileFormat,
    include_metadata: bool,
    max_depth: Option<usize>,
) -> String {
    let normalized = path.map(normalize_path_filter).unwrap_or_default();
    let mut files = indexed_files(view)
        .into_iter()
        .filter(|file| {
            normalized.is_empty()
                || file.path == normalized
                || file.path.starts_with(&format!("{normalized}/"))
        })
        .filter(|file| pattern.is_none_or(|pattern| glob_matches(pattern, &file.path)))
        .filter(|file| {
            max_depth.is_none_or(|depth| file.path.matches('/').count() < depth.clamp(1, 20))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    if files.is_empty() {
        return "No files found matching the criteria.".into();
    }
    let lines = match format {
        FileFormat::Flat => format_files_flat(&files, include_metadata),
        FileFormat::Grouped => format_files_grouped(&files, include_metadata),
        FileFormat::Tree => format_files_tree(&files, include_metadata),
    };
    bounded(lines, MAX_TEXT)
}

struct DiscoveredChanges {
    provenance: &'static str,
    paths: BTreeMap<String, Vec<(u32, u32)>>,
    deleted: BTreeSet<String>,
}

fn discover_changes(
    view: &ProjectView,
    base: &str,
    explicit: Option<&[String]>,
) -> DiscoveredChanges {
    if let Some(paths) = explicit {
        let paths = paths
            .iter()
            .take(64)
            .map(|path| (normalize_path_filter(path), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let deleted = paths
            .keys()
            .filter(|path| !view.root.join(path).exists())
            .cloned()
            .collect();
        return DiscoveredChanges {
            provenance: "explicit-paths",
            paths,
            deleted,
        };
    }
    if !base.is_empty() && base.len() <= 256 && !base.starts_with('-') {
        let verify = Command::new("git")
            .args(["-C"])
            .arg(&view.root)
            .args(["rev-parse", "--verify", &format!("{base}^{{commit}}")])
            .output();
        if let Ok(verify) = verify
            && verify.status.success()
        {
            let resolved = String::from_utf8_lossy(&verify.stdout).trim().to_owned();
            let names = Command::new("git")
                .args(["-C"])
                .arg(&view.root)
                .args([
                    "diff",
                    "--name-only",
                    "-z",
                    "--diff-filter=ACMRD",
                    &resolved,
                    "--",
                ])
                .output();
            let patch = Command::new("git")
                .args(["-C"])
                .arg(&view.root)
                .args([
                    "-c",
                    "core.quotepath=false",
                    "diff",
                    "--unified=0",
                    "--no-color",
                    &resolved,
                    "--",
                ])
                .output();
            let untracked = Command::new("git")
                .args(["-C"])
                .arg(&view.root)
                .args(["ls-files", "--others", "--exclude-standard", "-z"])
                .output();
            if let (Ok(names), Ok(patch), Ok(untracked)) = (names, patch, untracked)
                && names.status.success()
                && patch.status.success()
                && untracked.status.success()
            {
                let mut paths = nul_paths(&names.stdout)
                    .into_iter()
                    .map(|path| (path, Vec::new()))
                    .collect::<BTreeMap<_, _>>();
                for path in nul_paths(&untracked.stdout) {
                    paths.entry(path).or_default();
                }
                for (path, ranges) in diff_ranges(&String::from_utf8_lossy(&patch.stdout)) {
                    paths.entry(path).or_default().extend(ranges);
                }
                let deleted = paths
                    .keys()
                    .filter(|path| !view.root.join(path).exists())
                    .cloned()
                    .collect();
                return DiscoveredChanges {
                    provenance: "git-worktree",
                    paths,
                    deleted,
                };
            }
        }
    }
    let mut paths = BTreeMap::new();
    if let Ok(ledger) = LedgerStore::open(&view.root) {
        for event in ledger.events().iter().rev() {
            if let LedgerEventKind::EditApplied { paths: edited }
            | LedgerEventKind::EditObserved { paths: edited } = &event.kind
            {
                for path in edited.iter().take(64) {
                    paths.entry(normalize_path_filter(path)).or_default();
                }
                break;
            }
        }
    }
    let deleted = paths
        .keys()
        .filter(|path| !view.root.join(path).exists())
        .cloned()
        .collect();
    DiscoveredChanges {
        provenance: "ledger-fallback",
        paths,
        deleted,
    }
}

fn nul_paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| normalize_path_filter(&String::from_utf8_lossy(path)))
        .filter(|path| !path.is_empty())
        .collect()
}

fn diff_ranges(diff: &str) -> BTreeMap<String, Vec<(u32, u32)>> {
    let mut result = BTreeMap::new();
    let mut path = None;
    for line in diff.lines() {
        if let Some(value) = line.strip_prefix("+++ ") {
            path = value
                .strip_prefix("b/")
                .filter(|value| *value != "/dev/null")
                .map(normalize_path_filter);
        } else if line.starts_with("@@ ")
            && let Some(path) = &path
            && let Some((start, end)) = new_hunk_range(line)
        {
            result
                .entry(path.clone())
                .or_insert_with(Vec::new)
                .push((start, end));
        }
    }
    result
}

fn new_hunk_range(header: &str) -> Option<(u32, u32)> {
    let added = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))?;
    let mut values = added.trim_start_matches('+').split(',');
    let start = values.next()?.parse::<u32>().ok()?;
    let count = values
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    Some((start, start.saturating_add(count.saturating_sub(1))))
}

fn incoming_impact(
    view: &ProjectView,
    direct: &BTreeSet<NodeId>,
    depth: usize,
) -> BTreeMap<NodeId, usize> {
    let mut seen = direct
        .iter()
        .copied()
        .map(|node| (node, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut queue = direct
        .iter()
        .copied()
        .map(|node| (node, 0_usize))
        .collect::<VecDeque<_>>();
    while let Some((node, level)) = queue.pop_front() {
        if level >= depth {
            continue;
        }
        for edge_id in view.index.graph.incoming(node) {
            let Some(edge) = view.index.graph.edge(*edge_id) else {
                continue;
            };
            if view.index.graph.atom(edge.kind) != "contains" && !seen.contains_key(&edge.source) {
                seen.insert(edge.source, level + 1);
                queue.push_back((edge.source, level + 1));
            }
        }
    }
    for node in direct {
        seen.remove(node);
    }
    seen
}

fn is_test_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("/tests/")
        || path.contains("/__tests__/")
        || path.contains("/spec/")
        || path.starts_with("tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_test.go")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".spec.ts")
        || path.ends_with(".spec.tsx")
}

fn append_node_section(
    view: &ProjectView,
    sections: &mut Vec<String>,
    heading: &str,
    nodes: &BTreeSet<NodeId>,
    limit: usize,
) {
    if nodes.is_empty() {
        return;
    }
    sections.push(String::new());
    sections.push(format!("## {heading}"));
    sections.extend(
        nodes
            .iter()
            .take(limit)
            .map(|node| format_node(view, *node, None)),
    );
}

fn endpoint_candidates(
    view: &ProjectView,
    endpoint: &str,
    symbol: &str,
    nodes: &[NodeId],
) -> String {
    if nodes.is_empty() {
        return format!("Path {endpoint} symbol \"{symbol}\" was not found.");
    }
    let mut lines = vec![format!(
        "Path {endpoint} symbol \"{symbol}\" is ambiguous; pass {endpoint}File:"
    )];
    lines.extend(
        nodes
            .iter()
            .take(20)
            .map(|node| format_node(view, *node, None)),
    );
    lines.join("\n")
}

fn shortest_paths(
    view: &ProjectView,
    start: NodeId,
    target: NodeId,
    mode: PathMode,
    max_hops: usize,
    limit: usize,
) -> Vec<Vec<EdgeId>> {
    if start == target {
        return vec![Vec::new()];
    }
    let mut queue = VecDeque::from([(start, Vec::<EdgeId>::new(), BTreeSet::from([start]))]);
    let mut best_depth: BTreeMap<NodeId, usize> = BTreeMap::from([(start, 0)]);
    let mut results = Vec::new();
    let mut result_depth = None;
    while let Some((node, path, visited)) = queue.pop_front() {
        if path.len() >= max_hops || result_depth.is_some_and(|depth| path.len() >= depth) {
            continue;
        }
        let mut edges = view
            .index
            .graph
            .outgoing(node)
            .iter()
            .filter_map(|edge_id| {
                let edge = view.index.graph.edge(*edge_id)?;
                edge_allowed(view.index.graph.atom(edge.kind), mode)
                    .then_some((*edge_id, edge.target))
            })
            .collect::<Vec<_>>();
        edges.sort_by_key(|(edge_id, target)| {
            (
                node_path(view, *target).unwrap_or(""),
                view.index.graph.label(*target),
                view.index
                    .graph
                    .edge(*edge_id)
                    .map(|edge| view.index.graph.atom(edge.kind))
                    .unwrap_or(""),
            )
        });
        for (edge_id, next) in edges {
            if visited.contains(&next) {
                continue;
            }
            let next_depth = path.len() + 1;
            if best_depth
                .get(&next)
                .is_some_and(|depth| *depth < next_depth)
            {
                continue;
            }
            best_depth.insert(next, next_depth);
            let mut next_path = path.clone();
            next_path.push(edge_id);
            if next == target {
                result_depth.get_or_insert(next_depth);
                results.push(next_path);
                if results.len() == limit {
                    return results;
                }
            } else {
                let mut next_visited = visited.clone();
                next_visited.insert(next);
                queue.push_back((next, next_path, next_visited));
            }
        }
    }
    results
}

fn edge_allowed(kind: &str, mode: PathMode) -> bool {
    if kind == "contains" {
        return false;
    }
    match mode {
        PathMode::Execution => is_execution_edge(kind),
        PathMode::Dependency => !is_execution_edge(kind),
        PathMode::Any => true,
    }
}

fn symbol_nodes(view: &ProjectView, symbol: &str, file: Option<&str>) -> Vec<NodeId> {
    let wanted = symbol.trim().to_ascii_lowercase();
    let mut nodes = view
        .index
        .graph
        .nodes
        .iter()
        .filter(|node| view.index.graph.kind(node.id) != "file")
        .filter(|node| {
            let label = view.index.graph.label(node.id).to_ascii_lowercase();
            let qualified = view
                .index
                .qualified_names
                .get(&node.id)
                .map(|value| value.to_ascii_lowercase())
                .unwrap_or_default();
            label == wanted || qualified == wanted || qualified.ends_with(&format!("::{wanted}"))
        })
        .map(|node| node.id)
        .collect::<Vec<_>>();
    if let Some(file) = file {
        let normalized = normalize_path_filter(file).to_ascii_lowercase();
        let narrowed = nodes
            .iter()
            .copied()
            .filter(|node| {
                node_path(view, *node)
                    .is_some_and(|path| path.to_ascii_lowercase().ends_with(&normalized))
            })
            .collect::<Vec<_>>();
        if !narrowed.is_empty() {
            nodes = narrowed;
        }
    }
    nodes.sort();
    nodes
}

fn direct_trail(view: &ProjectView, node: NodeId) -> String {
    let collect = |edges: &[spectra_core::graph::EdgeId], incoming: bool| {
        edges
            .iter()
            .filter_map(|edge| view.index.graph.edge(*edge))
            .filter(|edge| is_execution_edge(view.index.graph.atom(edge.kind)))
            .map(|edge| if incoming { edge.source } else { edge.target })
            .filter(|related| *related != node)
            .map(|related| {
                let path = node_path(view, related).unwrap_or("<unknown>");
                let line = view
                    .index
                    .spans
                    .get(&related)
                    .map(|span| span.start_line)
                    .unwrap_or(0);
                format!("{} ({path}:{line})", view.index.graph.label(related))
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(12)
            .collect::<Vec<_>>()
    };
    let calls = collect(view.index.graph.outgoing(node), false);
    let callers = collect(view.index.graph.incoming(node), true);
    if calls.is_empty() && callers.is_empty() {
        return String::new();
    }
    let mut lines = vec!["**Trail**".into()];
    if !calls.is_empty() {
        lines.push(format!("**Calls →** {}", calls.join(", ")));
    }
    if !callers.is_empty() {
        lines.push(format!("**Called by ←** {}", callers.join(", ")));
    }
    lines.join("\n")
}

fn file_view(
    view: &ProjectView,
    file: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    symbols_only: bool,
) -> String {
    let matches = matching_files(view, file);
    if matches.len() > 1 {
        return format!(
            "\"{file}\" matches {} indexed files — pass a longer path:\n\n{}",
            matches.len(),
            matches
                .iter()
                .take(25)
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let Some(path) = matches.first() else {
        return format!("No indexed file matches \"{file}\".");
    };
    let nodes = nodes_in_file(view, path);
    let dependents = file_dependents(view, path);
    let dependency = if dependents.is_empty() {
        "no other indexed file depends on it".into()
    } else {
        format!(
            "used by {} file{}: {}",
            dependents.len(),
            if dependents.len() == 1 { "" } else { "s" },
            dependents
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let header = format!("**{path}** — {} symbols, {dependency}", nodes.len());
    if symbols_only || is_config_path(path) {
        let mut lines = vec![header, String::new()];
        for node in nodes.iter().take(200) {
            let line = view
                .index
                .spans
                .get(node)
                .map(|span| span.start_line)
                .unwrap_or(0);
            lines.push(format!(
                "- `{}` ({}) — :{line}",
                view.index.graph.label(*node),
                view.index.graph.kind(*node)
            ));
        }
        if is_config_path(path) {
            lines.push(String::new());
            lines.push("> Configuration values are withheld; indexed keys are shown above.".into());
        }
        return bounded(lines.join("\n"), MAX_TEXT);
    }
    let content = match read_indexed_file(view, path) {
        Ok(content) => content,
        Err(error) => return format!("{header}\n\n> Could not read current source: {error}"),
    };
    let source = content.split('\n').collect::<Vec<_>>();
    let start = offset.unwrap_or(1).max(1);
    if start > source.len() {
        return format!(
            "{header}\n\nOffset {start} is past the end ({} lines).",
            source.len()
        );
    }
    let limit = limit.unwrap_or(MAX_SOURCE_LINES).clamp(1, MAX_SOURCE_LINES);
    let numbered = source
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(limit)
        .map(|(index, line)| format!("{}\t{line}", index + 1))
        .collect::<Vec<_>>();
    bounded(format!("{header}\n\n{}", numbered.join("\n")), 38_000)
}

fn source_window(
    view: &ProjectView,
    path: &str,
    nodes: &[NodeId],
    max_lines: usize,
    max_chars: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    if is_config_path(path) {
        return Ok("> Configuration values withheld; use the indexed key symbols above.".into());
    }
    let content = read_indexed_file(view, path)?;
    let lines = content.split('\n').collect::<Vec<_>>();
    let start = nodes
        .iter()
        .filter_map(|node| view.index.spans.get(node))
        .map(|span| span.start_line.saturating_sub(2) as usize)
        .min()
        .unwrap_or(1)
        .max(1);
    let end = nodes
        .iter()
        .filter_map(|node| view.index.spans.get(node))
        .map(|span| span.end_line.saturating_add(2) as usize)
        .max()
        .unwrap_or(start)
        .min(lines.len())
        .min(start + max_lines.saturating_sub(1));
    let numbered = lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start - 1)
        .map(|(index, line)| format!("{}\t{line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(bounded(numbered, max_chars))
}

fn read_indexed_file(
    view: &ProjectView,
    relative: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let path = view.root.join(relative).canonicalize()?;
    if !path.starts_with(&view.root) || !path.is_file() {
        return Err("indexed path escaped the project root".into());
    }
    Ok(fs::read_to_string(path)?)
}

fn format_node(view: &ProjectView, node: NodeId, edge: Option<&str>) -> String {
    let path = node_path(view, node).unwrap_or("<unknown>");
    let line = view
        .index
        .spans
        .get(&node)
        .map(|span| span.start_line)
        .unwrap_or(0);
    let edge = edge
        .map(|edge| format!(" — via {edge}"))
        .unwrap_or_default();
    format!(
        "- {} ({}) — {path}:{line}{edge}",
        view.index.graph.label(node),
        view.index.graph.kind(node)
    )
}

fn node_path(view: &ProjectView, node: NodeId) -> Option<&str> {
    view.index.spans.get(&node).map(|span| span.path.as_str())
}

fn is_execution_edge(kind: &str) -> bool {
    matches!(
        kind,
        "calls" | "launches" | "routes_to" | "binds" | "renders"
    ) || kind.starts_with("uncertain_")
}

fn matching_files(view: &ProjectView, file: &str) -> Vec<String> {
    let wanted = normalize_path_filter(file).to_ascii_lowercase();
    let files = indexed_files(view);
    let exact = files
        .iter()
        .filter(|entry| entry.path.to_ascii_lowercase() == wanted)
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        return exact;
    }
    files
        .into_iter()
        .filter(|entry| entry.path.to_ascii_lowercase().ends_with(&wanted))
        .map(|entry| entry.path)
        .collect()
}

fn nodes_in_file(view: &ProjectView, path: &str) -> Vec<NodeId> {
    view.index
        .graph
        .nodes
        .iter()
        .filter(|node| view.index.graph.kind(node.id) != "file")
        .filter(|node| node_path(view, node.id) == Some(path))
        .map(|node| node.id)
        .collect()
}

fn file_dependents(view: &ProjectView, path: &str) -> BTreeSet<String> {
    view.index
        .graph
        .edges
        .iter()
        .filter(|edge| node_path(view, edge.target) == Some(path))
        .filter_map(|edge| node_path(view, edge.source))
        .filter(|source| *source != path)
        .map(str::to_owned)
        .collect()
}

struct IndexedFile {
    path: String,
    language: String,
    symbols: usize,
}

fn indexed_files(view: &ProjectView) -> Vec<IndexedFile> {
    view.index
        .graph
        .nodes
        .iter()
        .filter(|node| view.index.graph.kind(node.id) == "file")
        .map(|node| {
            let path = view
                .index
                .qualified_names
                .get(&node.id)
                .cloned()
                .unwrap_or_else(|| view.index.graph.label(node.id).to_owned());
            IndexedFile {
                language: language_for_path(&path),
                symbols: nodes_in_file(view, &path).len(),
                path,
            }
        })
        .collect()
}

fn language_for_path(path: &str) -> String {
    if path.ends_with(".app.src") {
        return "erlang".into();
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    supported_languages()
        .into_iter()
        .find(|language| language.extensions.contains(&extension))
        .map(|language| language.id.to_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn format_files_flat(files: &[IndexedFile], metadata: bool) -> String {
    let mut lines = vec![format!("**Files ({})**", files.len()), String::new()];
    for file in files {
        if metadata {
            lines.push(format!(
                "- {} ({}, {} symbols)",
                file.path, file.language, file.symbols
            ));
        } else {
            lines.push(format!("- {}", file.path));
        }
    }
    lines.join("\n")
}

fn format_files_grouped(files: &[IndexedFile], metadata: bool) -> String {
    let mut grouped: BTreeMap<&str, Vec<&IndexedFile>> = BTreeMap::new();
    for file in files {
        grouped.entry(&file.language).or_default().push(file);
    }
    let mut lines = vec![
        format!("**Files by Language ({} total)**", files.len()),
        String::new(),
    ];
    for (language, files) in grouped {
        lines.push(format!("**{language} ({})**", files.len()));
        for file in files {
            if metadata {
                lines.push(format!("- {} ({} symbols)", file.path, file.symbols));
            } else {
                lines.push(format!("- {}", file.path));
            }
        }
    }
    lines.join("\n")
}

fn format_files_tree(files: &[IndexedFile], metadata: bool) -> String {
    let mut lines = vec![
        format!("**Project Files ({})**", files.len()),
        String::new(),
    ];
    for file in files {
        let depth = file.path.matches('/').count();
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let suffix = if metadata {
            format!(" ({}, {} symbols)", file.language, file.symbols)
        } else {
            String::new()
        };
        lines.push(format!("{}- {name}{suffix}", "  ".repeat(depth)));
    }
    lines.join("\n")
}

fn normalize_path_filter(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_owned()
}

fn is_config_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("yml" | "yaml" | "properties")
    )
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    fn matches(
        pattern: &[u8],
        value: &[u8],
        p: usize,
        v: usize,
        memo: &mut [Vec<Option<bool>>],
    ) -> bool {
        if let Some(result) = memo[p][v] {
            return result;
        }
        let result = if p == pattern.len() {
            v == value.len()
        } else if pattern[p] == b'*' && pattern.get(p + 1) == Some(&b'*') {
            matches(pattern, value, p + 2, v, memo)
                || (v < value.len() && matches(pattern, value, p, v + 1, memo))
        } else if pattern[p] == b'*' {
            matches(pattern, value, p + 1, v, memo)
                || (v < value.len() && value[v] != b'/' && matches(pattern, value, p, v + 1, memo))
        } else if pattern[p] == b'?' {
            v < value.len() && value[v] != b'/' && matches(pattern, value, p + 1, v + 1, memo)
        } else {
            v < value.len() && pattern[p] == value[v] && matches(pattern, value, p + 1, v + 1, memo)
        };
        memo[p][v] = Some(result);
        result
    }
    matches(pattern, value, 0, 0, &mut memo)
}

fn bounded(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        let mut output = value
            .chars()
            .take(max_chars.saturating_sub(30))
            .collect::<String>();
        output.push_str("\n… output truncated by Spectra");
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spectra_core::{LedgerEventKind, LedgerSource};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spectra-{label}-{}-{unique}", std::process::id()))
    }

    fn efficiency_fixture(root: &Path) {
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("app.py"),
            "from helper import helper\ndef run():\n    helper()\n",
        )
        .unwrap();
        fs::write(
            root.join("helper.py"),
            "def helper():\n    leaf()\ndef leaf():\n    return 1\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/test_app.py"),
            "from app import run\ndef test_run():\n    run()\n",
        )
        .unwrap();
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-C"])
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn glob_matching_supports_codegraph_style_patterns() {
        assert!(glob_matches("*.rs", "lib.rs"));
        assert!(glob_matches("**/*.rs", "src/lib.rs"));
        assert!(glob_matches("src/?.rs", "src/a.rs"));
        assert!(!glob_matches("*.rs", "src/lib.rs"));
    }

    #[test]
    fn path_filters_normalize_client_spellings() {
        assert_eq!(normalize_path_filter("./src\\core/"), "src/core");
        assert_eq!(normalize_path_filter("/"), "");
    }

    #[test]
    fn query_pack_serves_source_relationships_files_and_status() {
        let root = temp_root("mcp-query-test");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("app.py"),
            "from helper import helper\ndef run():\n    helper()\n",
        )
        .unwrap();
        fs::write(root.join("helper.py"), "def helper():\n    return 1\n").unwrap();
        fs::write(
            root.join("application.properties"),
            "database.password=super-secret-value\n",
        )
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();

        assert!(search(&view, "run", Some("function"), 10).contains("app.py:2"));
        assert!(relationships(&view, "helper", None, Direction::Callers, 20).contains("run"));
        assert!(relationships(&view, "run", None, Direction::Callees, 20).contains("helper"));
        assert!(impact(&view, "helper", None, 2).contains("run"));
        let explored = explore(&view, "run helper", 4);
        assert!(explored.contains("2\tdef run"));
        assert!(explored.contains("run —calls→ helper"));
        assert!(
            node_view(
                &view,
                NodeViewOptions {
                    symbol: Some("run"),
                    file: None,
                    line: None,
                    include_code: true,
                    offset: None,
                    limit: None,
                    symbols_only: false,
                }
            )
            .contains("3\t    helper()")
        );
        assert!(
            node_view(
                &view,
                NodeViewOptions {
                    symbol: None,
                    file: Some("app.py"),
                    line: None,
                    include_code: false,
                    offset: Some(2),
                    limit: Some(2),
                    symbols_only: false,
                }
            )
            .contains("2\tdef run")
        );
        assert!(
            files(&view, None, Some("*.py"), FileFormat::Grouped, true, None)
                .contains("python (2)")
        );
        let config = node_view(
            &view,
            NodeViewOptions {
                symbol: None,
                file: Some("application.properties"),
                line: None,
                include_code: false,
                offset: None,
                limit: None,
                symbols_only: false,
            },
        );
        assert!(config.contains("database.password"));
        assert!(!config.contains("super-secret-value"));
        assert!(status(&view).contains("**Files indexed:** 3"));

        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn brief_is_bounded_and_never_borrows_another_session_state() {
        let root = temp_root("brief-test");
        efficiency_fixture(&root);
        let first = LedgerSource {
            harness: "custom".into(),
            session_id: "one".into(),
        };
        let second = LedgerSource {
            harness: "custom".into(),
            session_id: "two".into(),
        };
        LedgerStore::transaction(&root, |ledger| {
            ledger.append_for(
                first.clone(),
                LedgerEventKind::EditObserved {
                    paths: vec!["app.py".into()],
                },
            )?;
            ledger.append_for(
                second,
                LedgerEventKind::Blocked {
                    reason: "waiting for fixture".into(),
                },
            )?;
            Ok(())
        })
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();
        let shared = brief(
            &view,
            BriefOptions {
                query: "TOKEN=do-not-echo",
                token_budget: 600,
                include_source: false,
                source: None,
            },
        );
        assert!(shared.contains("edit app.py"));
        assert!(shared.contains("waiting for fixture"));
        assert!(shared.contains("goal=[REDACTED]"));
        assert!(!shared.contains("Editing"));
        assert!(!shared.contains("AwaitingAuthorization"));
        assert!(shared.chars().count().div_ceil(4) <= 600);
        let session = brief(
            &view,
            BriefOptions {
                query: "resume run",
                token_budget: 600,
                include_source: true,
                source: Some(first),
            },
        );
        assert!(session.contains("Editing"));
        assert!(session.contains("app.py"));
        assert!(session.contains("2\tdef run"));
        assert!(!session.contains("super-secret-value"));
        assert!(session.chars().count().div_ceil(4) <= 600);
        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changes_cover_git_worktree_explicit_paths_tests_and_ledger_fallback() {
        let root = temp_root("changes-test");
        efficiency_fixture(&root);
        git(&root, &["init", "-q"]);
        git(&root, &["add", "."]);
        git(
            &root,
            &[
                "-c",
                "user.name=Spectra",
                "-c",
                "user.email=spectra@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        fs::write(
            root.join("app.py"),
            "from helper import helper\ndef run():\n    helper()\n    return 2\n",
        )
        .unwrap();
        git(&root, &["add", "app.py"]);
        fs::write(
            root.join("app.py"),
            "from helper import helper\ndef run():\n    helper()\n    return 3\n",
        )
        .unwrap();
        fs::remove_file(root.join("helper.py")).unwrap();
        fs::write(root.join("new.rs"), "pub fn new_symbol() {}\n").unwrap();
        fs::write(
            root.join("application.properties"),
            "database.password=rotated-secret-value\n",
        )
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();
        let worktree = changes(
            &view,
            ChangeOptions {
                base: "HEAD",
                paths: None,
                depth: 2,
                include_tests: true,
                token_budget: 800,
            },
        );
        assert!(worktree.contains("provenance=git-worktree"));
        assert!(worktree.contains("app.py"));
        assert!(worktree.contains("helper.py deleted"));
        assert!(worktree.contains("new.rs"));
        assert!(worktree.contains("application.properties"));
        assert!(!worktree.contains("return 3"));
        assert!(!worktree.contains("rotated-secret-value"));
        assert!(worktree.chars().count().div_ceil(4) <= 800);
        let explicit_paths = vec!["app.py".into()];
        let explicit = changes(
            &view,
            ChangeOptions {
                base: "HEAD",
                paths: Some(&explicit_paths),
                depth: 2,
                include_tests: true,
                token_budget: 800,
            },
        );
        assert!(explicit.contains("provenance=explicit-paths"));
        assert!(explicit.contains("run (function)"));
        assert!(explicit.contains("test_run"));
        assert!(explicit.contains("1. distance="));
        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();

        let fallback_root = temp_root("changes-ledger-test");
        efficiency_fixture(&fallback_root);
        LedgerStore::transaction(&fallback_root, |ledger| {
            ledger.append(LedgerEventKind::EditObserved {
                paths: vec!["app.py".into()],
            })?;
            Ok(())
        })
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, fallback_root.to_str()).unwrap();
        let fallback = changes(
            &view,
            ChangeOptions {
                base: "HEAD",
                paths: None,
                depth: 2,
                include_tests: true,
                token_budget: 800,
            },
        );
        assert!(fallback.contains("provenance=ledger-fallback"));
        assert!(fallback.contains("app.py"));
        drop(view);
        drop(autosync);
        fs::remove_dir_all(fallback_root).unwrap();
    }

    #[test]
    fn typed_paths_are_directed_deterministic_and_report_ambiguity() {
        let root = temp_root("path-test");
        efficiency_fixture(&root);
        fs::write(
            root.join("duplicate_a.py"),
            "def duplicate():\n    return 1\n",
        )
        .unwrap();
        fs::write(
            root.join("duplicate_b.py"),
            "def duplicate():\n    return 2\n",
        )
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();
        let forward = typed_paths(
            &view,
            PathOptions {
                from: "run",
                to: "leaf",
                from_file: None,
                to_file: None,
                mode: PathMode::Execution,
                max_hops: 8,
            },
        );
        assert!(forward.contains("Path 1 (2 hops)"));
        assert!(forward.contains("-calls→"));
        assert_eq!(
            forward,
            typed_paths(
                &view,
                PathOptions {
                    from: "run",
                    to: "leaf",
                    from_file: None,
                    to_file: None,
                    mode: PathMode::Execution,
                    max_hops: 8,
                },
            )
        );
        let reverse = typed_paths(
            &view,
            PathOptions {
                from: "leaf",
                to: "run",
                from_file: None,
                to_file: None,
                mode: PathMode::Execution,
                max_hops: 8,
            },
        );
        assert!(reverse.contains("a reverse path exists"));
        let ambiguous = typed_paths(
            &view,
            PathOptions {
                from: "duplicate",
                to: "run",
                from_file: None,
                to_file: None,
                mode: PathMode::Any,
                max_hops: 8,
            },
        );
        assert!(ambiguous.contains("ambiguous"));
        assert!(ambiguous.contains("duplicate_a.py"));
        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adaptive_context_is_budgeted_classified_and_session_deduplicated() {
        let root = temp_root("adaptive-context-test");
        efficiency_fixture(&root);
        let source = LedgerSource {
            harness: "custom".into(),
            session_id: "one".into(),
        };
        LedgerStore::transaction(&root, |ledger| {
            ledger.append_for(
                source.clone(),
                LedgerEventKind::EditObserved {
                    paths: vec!["app.py".into()],
                },
            )?;
            Ok(())
        })
        .unwrap();
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();
        let first = context_packet(
            &view,
            ContextOptions {
                query: "How does run reach leaf?",
                token_budget: 256,
                intent: ContextIntent::Auto,
                delivery: Delivery::Delta,
                source: Some(source.clone()),
                cursor: None,
                map_requested: false,
            },
        )
        .unwrap();
        assert!(first.text.contains("intent=flow"));
        assert!(first.text.contains("image_cost=none"));
        assert!(first.text.contains("state S"));
        assert!(first.text.contains("A1"));
        assert!(estimate_tokens(&first.text) <= 256);
        let duplicate = context_packet(
            &view,
            ContextOptions {
                query: "How does run reach leaf?",
                token_budget: 256,
                intent: ContextIntent::Auto,
                delivery: Delivery::Delta,
                source: Some(source.clone()),
                cursor: None,
                map_requested: false,
            },
        )
        .unwrap();
        assert!(duplicate.text.contains("unchanged sequence="));
        assert!(!duplicate.text.contains("private source body"));
        let full = context_packet(
            &view,
            ContextOptions {
                query: "How does run reach leaf?",
                token_budget: 256,
                intent: ContextIntent::Flow,
                delivery: Delivery::Full,
                source: Some(source),
                cursor: None,
                map_requested: false,
            },
        )
        .unwrap();
        assert!(full.text.contains("delivery=full"));
        assert!(full.text.contains("A1"));
        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_cursor_rejects_a_changed_query() {
        let root = temp_root("context-cursor-test");
        efficiency_fixture(&root);
        let autosync = AutoSync::default();
        let view = open_project(&autosync, root.to_str()).unwrap();
        let first = context_packet(
            &view,
            ContextOptions {
                query: "inspect implementation run helper leaf",
                token_budget: 128,
                intent: ContextIntent::Inspect,
                delivery: Delivery::Full,
                source: None,
                cursor: None,
                map_requested: false,
            },
        )
        .unwrap();
        let cursor = first
            .text
            .split("next=")
            .nth(1)
            .expect("bounded packet has a continuation")
            .trim();
        let error = context_packet(
            &view,
            ContextOptions {
                query: "different query",
                token_budget: 128,
                intent: ContextIntent::Inspect,
                delivery: Delivery::Full,
                source: None,
                cursor: Some(cursor),
                map_requested: false,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("cursor_stale"));
        drop(view);
        drop(autosync);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn frozen_efficiency_scenarios_reduce_median_tool_calls_by_forty_percent() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/fixtures/efficiency-tool-scenarios.json"
        ))
        .unwrap();
        assert_eq!(fixture["version"], 1);
        let mut reductions = fixture["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|scenario| {
                assert!(!scenario["required_facts"].as_array().unwrap().is_empty());
                let baseline = scenario["baseline_calls"].as_f64().unwrap();
                let composite = scenario["composite_calls"].as_f64().unwrap();
                1.0 - composite / baseline
            })
            .collect::<Vec<_>>();
        reductions.sort_by(f64::total_cmp);
        assert!(reductions[reductions.len() / 2] >= 0.40);
    }
}
