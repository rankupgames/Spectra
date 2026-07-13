use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use spectra_core::{
    CodeIndex, IndexReport, SelectionOptions, graph::NodeId, select_subgraph, supported_languages,
};

use crate::autosync::{AutoSync, SyncSnapshot};

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
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spectra-mcp-query-test-{}-{unique}",
            std::process::id()
        ));
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
}
