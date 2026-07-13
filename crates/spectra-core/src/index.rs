use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node as SyntaxNode, Parser};

use crate::{
    Error, Result,
    graph::{NodeId, PackedGraph},
};

pub const INDEX_VERSION: u32 = 1;
const INDEX_PATH: &str = ".spectra/index-v1.json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug)]
pub struct CodeIndex {
    pub graph: PackedGraph,
    pub spans: BTreeMap<NodeId, SourceSpan>,
    pub qualified_names: BTreeMap<NodeId, String>,
    pub version: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexReport {
    pub files: usize,
    pub changed: usize,
    pub removed: usize,
    pub nodes: usize,
    pub edges: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexCache {
    version: u32,
    files: BTreeMap<String, CachedFile>,
}

impl Default for IndexCache {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedFile {
    hash: u64,
    nodes: Vec<CachedNode>,
    calls: Vec<PendingCall>,
    implementations: Vec<PendingImplementation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedNode {
    id: u32,
    kind: String,
    label: String,
    qualified_name: String,
    start_line: u32,
    end_line: u32,
    parent: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingCall {
    source: u32,
    name: String,
    line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingImplementation {
    source: u32,
    trait_name: String,
}

impl CodeIndex {
    pub fn refresh(project: &Path) -> Result<(Self, IndexReport)> {
        let project = project
            .canonicalize()
            .map_err(|error| Error::InvalidProject(format!("{}: {error}", project.display())))?;
        if !project.is_dir() {
            return Err(Error::InvalidProject(format!(
                "{} is not a directory",
                project.display()
            )));
        }

        let cache_path = project.join(INDEX_PATH);
        let mut cache = load_cache(&cache_path).unwrap_or_default();
        if cache.version != INDEX_VERSION {
            cache = IndexCache::default();
        }

        let files = rust_files(&project)?;
        let mut live = BTreeMap::new();
        let mut changed = 0;
        for path in files {
            let relative = normalize_path(path.strip_prefix(&project).unwrap_or(&path));
            let source = fs::read_to_string(&path)?;
            let hash = stable_hash(source.as_bytes());
            let cached = cache.files.remove(&relative);
            let entry = match cached {
                Some(entry) if entry.hash == hash => entry,
                _ => {
                    changed += 1;
                    parse_file(&relative, &source, hash)?
                }
            };
            live.insert(relative, entry);
        }
        let removed = cache.files.len();
        cache.files = live;
        save_cache(&cache_path, &cache)?;

        let index = assemble(&cache)?;
        let report = IndexReport {
            files: cache.files.len(),
            changed,
            removed,
            nodes: index.graph.nodes.len(),
            edges: index.graph.edges.len(),
        };
        Ok((index, report))
    }
}

fn load_cache(path: &Path) -> Result<IndexCache> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn save_cache(path: &Path, cache: &IndexCache) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::InvalidProject("index has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join("index-v1.json.tmp");
    fs::write(&temporary, serde_json::to_vec(cache)?)?;
    // Windows does not replace an existing destination with rename.
    if cfg!(windows) && path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn rust_files(project: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkBuilder::new(project)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build()
    {
        let entry = entry.map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

fn parse_file(path: &str, source: &str, hash: u64) -> Result<CachedFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|error| Error::Parse(error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::Parse(format!("parser returned no tree for {path}")))?;

    let mut file = CachedFile {
        hash,
        nodes: Vec::new(),
        calls: Vec::new(),
        implementations: Vec::new(),
    };
    file.nodes.push(CachedNode {
        id: 0,
        kind: "file".into(),
        label: path.rsplit('/').next().unwrap_or(path).into(),
        qualified_name: path.into(),
        start_line: 1,
        end_line: source.lines().count().max(1) as u32,
        parent: None,
    });
    let mut scopes = Vec::new();
    visit(
        tree.root_node(),
        source.as_bytes(),
        &mut file,
        0,
        None,
        &mut scopes,
    );
    Ok(file)
}

fn visit(
    syntax: SyntaxNode<'_>,
    source: &[u8],
    file: &mut CachedFile,
    parent: u32,
    owner: Option<u32>,
    scopes: &mut Vec<String>,
) {
    let kind = syntax.kind();
    let mapped = match kind {
        "mod_item" => Some("module"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "trait_item" => Some("trait"),
        "impl_item" => Some("impl"),
        "function_item" => Some(
            if scopes
                .last()
                .is_some_and(|scope| scope.starts_with("impl ") || scope.starts_with("trait "))
            {
                "method"
            } else {
                "function"
            },
        ),
        "type_item" => Some("type_alias"),
        "const_item" => Some("constant"),
        "static_item" => Some("static"),
        "macro_definition" => Some("macro"),
        "use_declaration" => Some("import"),
        _ => None,
    };

    let mut next_parent = parent;
    let mut next_owner = owner;
    let mut pushed_scope = false;
    if let Some(mapped_kind) = mapped {
        let label = node_label(syntax, source, mapped_kind);
        let qualified_name = if scopes.is_empty() {
            label.clone()
        } else {
            format!("{}::{}", scopes.join("::"), label)
        };
        let id = file.nodes.len() as u32;
        let position = syntax.start_position();
        let end = syntax.end_position();
        file.nodes.push(CachedNode {
            id,
            kind: mapped_kind.into(),
            label: label.clone(),
            qualified_name,
            start_line: position.row as u32 + 1,
            end_line: end.row as u32 + 1,
            parent: Some(parent),
        });
        next_parent = id;
        if matches!(mapped_kind, "function" | "method") {
            next_owner = Some(id);
        }
        if matches!(
            mapped_kind,
            "module" | "struct" | "enum" | "trait" | "impl" | "function" | "method"
        ) {
            let scope = if mapped_kind == "impl" {
                format!("impl {label}")
            } else if mapped_kind == "trait" {
                format!("trait {label}")
            } else {
                label.clone()
            };
            scopes.push(scope);
            pushed_scope = true;
        }
        if mapped_kind == "impl" {
            if let Some(trait_node) = syntax.child_by_field_name("trait") {
                let trait_name = text(trait_node, source)
                    .rsplit("::")
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                if !trait_name.is_empty() {
                    file.implementations.push(PendingImplementation {
                        source: id,
                        trait_name,
                    });
                }
            }
        }
    }

    if kind == "call_expression" {
        if let (Some(owner), Some(function)) = (next_owner, syntax.child_by_field_name("function"))
        {
            let raw = text(function, source);
            let name = raw
                .rsplit([':', '.'])
                .find(|part| !part.is_empty())
                .unwrap_or(raw)
                .trim();
            if is_identifier(name) {
                file.calls.push(PendingCall {
                    source: owner,
                    name: name.into(),
                    line: syntax.start_position().row as u32 + 1,
                });
            }
        }
    }

    let mut cursor = syntax.walk();
    for child in syntax.children(&mut cursor) {
        visit(child, source, file, next_parent, next_owner, scopes);
    }
    if pushed_scope {
        scopes.pop();
    }
}

fn node_label(node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> String {
    if mapped_kind == "import" {
        return truncate(text(node, source).trim().trim_end_matches(';'), 72);
    }
    if mapped_kind == "impl" {
        if let Some(ty) = node.child_by_field_name("type") {
            return truncate(text(ty, source).trim(), 56);
        }
    }
    node.child_by_field_name("name")
        .map(|name| text(name, source).to_owned())
        .unwrap_or_else(|| mapped_kind.to_owned())
}

fn assemble(cache: &IndexCache) -> Result<CodeIndex> {
    let mut graph = PackedGraph::default();
    let mut spans = BTreeMap::new();
    let mut qualified_names = BTreeMap::new();
    let mut ids = HashMap::new();

    for (path, file) in &cache.files {
        for node in &file.nodes {
            let id = graph.add_node(&node.kind, &node.label);
            ids.insert((path.clone(), node.id), id);
            spans.insert(
                id,
                SourceSpan {
                    path: path.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                },
            );
            qualified_names.insert(id, node.qualified_name.clone());
        }
    }
    for (path, file) in &cache.files {
        for node in &file.nodes {
            if let Some(parent) = node.parent {
                graph.add_edge(
                    ids[&(path.clone(), parent)],
                    ids[&(path.clone(), node.id)],
                    "contains",
                )?;
            }
        }
    }

    let mut definitions: HashMap<String, Vec<NodeId>> = HashMap::new();
    for node in &graph.nodes {
        let kind = graph.atom(node.kind);
        if matches!(
            kind,
            "function" | "method" | "trait" | "struct" | "enum" | "module"
        ) {
            definitions
                .entry(graph.atom(node.label).to_ascii_lowercase())
                .or_default()
                .push(node.id);
        }
    }
    for (path, file) in &cache.files {
        for pending in &file.calls {
            let source = ids[&(path.clone(), pending.source)];
            match definitions
                .get(&pending.name.to_ascii_lowercase())
                .map(Vec::as_slice)
            {
                Some([target]) => {
                    graph.add_edge(source, *target, "calls")?;
                }
                Some(candidates) if !candidates.is_empty() => {
                    let boundary = graph.add_node("boundary", &format!("?{}", pending.name));
                    spans.insert(
                        boundary,
                        SourceSpan {
                            path: path.clone(),
                            start_line: pending.line,
                            end_line: pending.line,
                        },
                    );
                    qualified_names.insert(boundary, pending.name.clone());
                    graph.add_edge(source, boundary, "uncertain_call")?;
                }
                _ => {}
            }
        }
        for pending in &file.implementations {
            if let Some([target]) = definitions
                .get(&pending.trait_name.to_ascii_lowercase())
                .map(Vec::as_slice)
            {
                graph.add_edge(ids[&(path.clone(), pending.source)], *target, "implements")?;
            }
        }
    }
    graph.validate()?;
    Ok(CodeIndex {
        graph,
        spans,
        qualified_names,
        version: INDEX_VERSION,
    })
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn text<'a>(node: SyntaxNode<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}
fn is_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
}
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extracts_rust_structure_and_calls() {
        let file = parse_file("src/lib.rs", "trait Run { fn run(&self); } struct App; impl Run for App { fn run(&self) { helper(); } } fn helper() {}", 1).unwrap();
        assert!(
            file.nodes
                .iter()
                .any(|node| node.kind == "trait" && node.label == "Run")
        );
        assert!(
            file.nodes
                .iter()
                .any(|node| node.kind == "method" && node.label == "run")
        );
        assert_eq!(file.calls[0].name, "helper");
    }

    #[test]
    fn refresh_reuses_unchanged_files_and_detects_changes_and_deletions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spectra-index-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        let source = root.join("src/lib.rs");
        fs::write(&source, "pub fn first() {}\n").unwrap();
        let (_, cold) = CodeIndex::refresh(&root).unwrap();
        let (_, warm) = CodeIndex::refresh(&root).unwrap();
        assert_eq!((cold.changed, warm.changed), (1, 0));
        fs::write(&source, "pub fn second() {}\n").unwrap();
        let (changed_index, changed) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(changed.changed, 1);
        assert!(
            changed_index
                .graph
                .nodes
                .iter()
                .any(|node| changed_index.graph.atom(node.label) == "second")
        );
        fs::remove_file(source).unwrap();
        let (_, removed) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(removed.removed, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
