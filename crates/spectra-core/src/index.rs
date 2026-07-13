use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use tree_sitter::{Node as SyntaxNode, Parser};

use crate::{
    Error, Result,
    adapters::{self, LanguageAdapter, Scope},
    graph::{NodeId, PackedGraph},
};

pub const INDEX_VERSION: u32 = 3;
const INDEX_PATH: &str = ".spectra/index-v3.json";

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
    language: String,
    nodes: Vec<CachedNode>,
    edges: Vec<PendingEdge>,
    local_edges: Vec<CachedEdge>,
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
struct PendingEdge {
    source: u32,
    target_name: String,
    kind: String,
    line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedEdge {
    source: u32,
    target: u32,
    kind: String,
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

        let files = source_files(&project)?;
        let mut live = BTreeMap::new();
        let mut changed = 0;
        for path in files {
            let adapter = adapters::for_path(&path).expect("source file has an adapter");
            let relative = normalize_path(path.strip_prefix(&project).unwrap_or(&path));
            let source = fs::read_to_string(&path)?;
            let hash = stable_hash(source.as_bytes());
            let cached = cache.files.remove(&relative);
            let entry = match cached {
                Some(entry) if entry.hash == hash && entry.language == adapter.id() => entry,
                _ => {
                    changed += 1;
                    parse_file(adapter, &relative, &source, hash)?
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
    let destination = if path.is_symlink() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };
    let mut file = AtomicWriteFile::open(destination)?;
    file.write_all(&serde_json::to_vec(cache)?)?;
    file.commit()?;
    Ok(())
}

fn source_files(project: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkBuilder::new(project)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build()
    {
        let entry = entry.map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && adapters::for_path(entry.path()).is_some()
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

fn parse_file(
    adapter: &dyn LanguageAdapter,
    path: &str,
    source: &str,
    hash: u64,
) -> Result<CachedFile> {
    let mut parser = Parser::new();
    parser
        .set_language(&adapter.language(Path::new(path)))
        .map_err(|error| Error::Parse(error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::Parse(format!("parser returned no tree for {path}")))?;

    let mut file = CachedFile {
        hash,
        language: adapter.id().into(),
        nodes: Vec::new(),
        edges: Vec::new(),
        local_edges: Vec::new(),
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
    let mut component = None;
    let mut routes = Vec::new();
    for symbol in adapter.file_symbols(Path::new(path), source) {
        let id = file.nodes.len() as u32;
        file.nodes.push(CachedNode {
            id,
            kind: symbol.kind.into(),
            label: symbol.label.clone(),
            qualified_name: symbol.label,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            parent: Some(0),
        });
        for relation in symbol.relations {
            file.edges.push(PendingEdge {
                source: id,
                target_name: relation.target,
                kind: relation.kind.into(),
                line: symbol.start_line,
            });
        }
        if symbol.kind == "component" {
            component = Some(id);
        } else if symbol.kind == "route" {
            routes.push(id);
        }
    }
    if let Some(component) = component {
        file.local_edges
            .extend(routes.into_iter().map(|route| CachedEdge {
                source: route,
                target: component,
                kind: "routes_to".into(),
            }));
    }

    let syntax_parent = component.unwrap_or(0);
    let mut scopes = Vec::new();
    visit(
        adapter,
        tree.root_node(),
        source.as_bytes(),
        &mut file,
        VisitContext {
            parent: syntax_parent,
            owner: None,
            line_offset: 0,
        },
        &mut scopes,
    );
    for region in adapter.embedded_regions(source) {
        if region.start_byte >= region.end_byte
            || !source.is_char_boundary(region.start_byte)
            || !source.is_char_boundary(region.end_byte)
        {
            continue;
        }
        let Some(embedded_adapter) = adapters::for_id(region.language) else {
            continue;
        };
        let fragment = &source[region.start_byte..region.end_byte];
        let mut embedded_parser = Parser::new();
        let embedded_path = match region.language {
            "typescript" => Path::new("embedded.ts"),
            "javascript" => Path::new("embedded.js"),
            _ => Path::new("embedded.txt"),
        };
        embedded_parser
            .set_language(&embedded_adapter.language(embedded_path))
            .map_err(|error| Error::Parse(error.to_string()))?;
        let embedded_tree = embedded_parser
            .parse(fragment, None)
            .ok_or_else(|| Error::Parse(format!("embedded parser returned no tree for {path}")))?;
        let line_offset = source[..region.start_byte]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
        let mut embedded_scopes = component
            .and_then(|id| file.nodes.get(id as usize))
            .map(|node| {
                vec![Scope {
                    kind: "component",
                    label: node.label.clone(),
                }]
            })
            .unwrap_or_default();
        visit(
            embedded_adapter,
            embedded_tree.root_node(),
            fragment.as_bytes(),
            &mut file,
            VisitContext {
                parent: syntax_parent,
                owner: None,
                line_offset,
            },
            &mut embedded_scopes,
        );
    }
    Ok(file)
}

#[derive(Clone, Copy)]
struct VisitContext {
    parent: u32,
    owner: Option<u32>,
    line_offset: u32,
}

fn visit(
    adapter: &dyn LanguageAdapter,
    syntax: SyntaxNode<'_>,
    source: &[u8],
    file: &mut CachedFile,
    context: VisitContext,
    scopes: &mut Vec<Scope>,
) {
    let mapped = adapter.classify(syntax, source, scopes);

    let mut next = context;
    let mut pushed_scope = false;
    if let Some(mapped_kind) = mapped
        && let Some(label) = adapter.label(syntax, source, mapped_kind)
    {
        let qualified_name = if scopes.is_empty() {
            label.clone()
        } else {
            format!(
                "{}::{}",
                scopes
                    .iter()
                    .map(|scope| scope.label.as_str())
                    .collect::<Vec<_>>()
                    .join("::"),
                label
            )
        };
        let id = file.nodes.len() as u32;
        let position = syntax.start_position();
        let end = syntax.end_position();
        file.nodes.push(CachedNode {
            id,
            kind: mapped_kind.into(),
            label: label.clone(),
            qualified_name,
            start_line: position.row as u32 + 1 + context.line_offset,
            end_line: end.row as u32 + 1 + context.line_offset,
            parent: Some(context.parent),
        });
        next.parent = id;
        if matches!(mapped_kind, "function" | "method") {
            next.owner = Some(id);
        }
        for relation in adapter.relations(syntax, source) {
            file.edges.push(PendingEdge {
                source: id,
                target_name: relation.target,
                kind: relation.kind.into(),
                line: position.row as u32 + 1 + context.line_offset,
            });
        }
        if adapter.opens_scope(mapped_kind) {
            let scope_label = if mapped_kind == "impl" {
                format!("impl {label}")
            } else if mapped_kind == "trait" {
                format!("trait {label}")
            } else {
                label
            };
            scopes.push(Scope {
                kind: mapped_kind,
                label: scope_label,
            });
            pushed_scope = true;
        }
    }

    if let (Some(owner), Some(name)) = (next.owner, adapter.call_name(syntax, source))
        && is_identifier(&name)
    {
        file.edges.push(PendingEdge {
            source: owner,
            target_name: name,
            kind: "calls".into(),
            line: syntax.start_position().row as u32 + 1 + context.line_offset,
        });
    }

    let mut cursor = syntax.walk();
    for child in syntax.children(&mut cursor) {
        visit(adapter, child, source, file, next, scopes);
    }
    if pushed_scope {
        scopes.pop();
    }
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
        for edge in &file.local_edges {
            graph.add_edge(
                ids[&(path.clone(), edge.source)],
                ids[&(path.clone(), edge.target)],
                &edge.kind,
            )?;
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
            "file"
                | "function"
                | "method"
                | "class"
                | "interface"
                | "trait"
                | "struct"
                | "enum"
                | "module"
                | "type_alias"
                | "component"
        ) {
            definitions
                .entry(graph.atom(node.label).to_ascii_lowercase())
                .or_default()
                .push(node.id);
        }
    }
    for (path, file) in &cache.files {
        for pending in &file.edges {
            let source = ids[&(path.clone(), pending.source)];
            match definitions
                .get(&pending.target_name.to_ascii_lowercase())
                .map(Vec::as_slice)
            {
                Some([target]) => {
                    let kind = if pending.kind == "inherits" {
                        let source_kind = graph.kind(source);
                        let target_kind = graph.kind(*target);
                        if source_kind == "interface" {
                            "extends"
                        } else if matches!(target_kind, "interface" | "trait") {
                            "implements"
                        } else {
                            "extends"
                        }
                    } else {
                        &pending.kind
                    };
                    graph.add_edge(source, *target, kind)?;
                }
                Some(candidates) if !candidates.is_empty() => {
                    let boundary = graph.add_node("boundary", &format!("?{}", pending.target_name));
                    spans.insert(
                        boundary,
                        SourceSpan {
                            path: path.clone(),
                            start_line: pending.line,
                            end_line: pending.line,
                        },
                    );
                    qualified_names.insert(boundary, pending.target_name.clone());
                    graph.add_edge(source, boundary, &format!("uncertain_{}", pending.kind))?;
                }
                _ => {}
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
fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| matches!(ch, '_' | '$') || ch.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn parsed(path: &str, source: &str) -> CachedFile {
        let adapter = adapters::for_path(Path::new(path)).expect("test adapter");
        parse_file(adapter, path, source, 1).unwrap()
    }

    fn has_node(file: &CachedFile, kind: &str, label: &str) -> bool {
        file.nodes
            .iter()
            .any(|node| node.kind == kind && node.label == label)
    }

    fn has_edge(file: &CachedFile, kind: &str, target: &str) -> bool {
        file.edges
            .iter()
            .any(|edge| edge.kind == kind && edge.target_name == target)
    }

    fn has_local_edge(file: &CachedFile, kind: &str, source: &str, target: &str) -> bool {
        file.local_edges.iter().any(|edge| {
            edge.kind == kind
                && file.nodes[edge.source as usize].label == source
                && file.nodes[edge.target as usize].label == target
        })
    }

    #[test]
    fn extracts_rust_structure_and_calls() {
        let file = parsed(
            "src/lib.rs",
            "trait Run { fn run(&self); } struct App; impl Run for App { fn run(&self) { helper(); } } fn helper() {}",
        );
        assert!(has_node(&file, "trait", "Run"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "implements", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_python_structure_inheritance_and_calls() {
        let file = parsed(
            "app.py",
            "from helpers import helper\nclass Base: pass\nclass App(Base):\n    def run(self): helper()\ndef helper(): pass\n",
        );
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "extends", "Base"));
        assert!(has_edge(&file, "imports", "helper"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_javascript_named_and_arrow_functions() {
        let file = parsed(
            "app.js",
            "import { helper } from './helpers.js';\nclass Base {}\nclass App extends Base { run() { helper(); } }\nconst helper = () => {};\n",
        );
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_node(&file, "function", "helper"));
        assert!(has_edge(&file, "extends", "Base"));
        assert!(has_edge(&file, "imports", "helper"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_typescript_interfaces_and_calls() {
        let file = parsed(
            "app.ts",
            "interface Run { run(): void }\nclass App implements Run { run() { helper(); } }\nfunction helper() {}\n",
        );
        assert!(has_node(&file, "interface", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "implements", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_go_types_methods_and_calls() {
        let file = parsed(
            "app.go",
            "package app\ntype App struct{}\nfunc (App) Run() { helper() }\nfunc helper() {}\n",
        );
        assert!(has_node(&file, "struct", "App"));
        assert!(has_node(&file, "method", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_java_types_implementations_and_calls() {
        let file = parsed(
            "App.java",
            "interface Run { void run(); } class App implements Run { public void run() { helper(); } static void helper() {} }",
        );
        assert!(has_node(&file, "interface", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "implements", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_c_types_includes_and_calls() {
        let file = parsed(
            "app.c",
            "#include \"helper.h\"\nstruct App { int value; };\nvoid run(void) { helper(); }\nvoid helper(void) {}\n",
        );
        assert!(has_node(&file, "struct", "App"));
        assert!(has_node(&file, "function", "run"));
        assert!(has_edge(&file, "imports", "helper.h"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_cpp_classes_inheritance_methods_and_calls() {
        let file = parsed(
            "app.cpp",
            "class Base {}; class App : public Base { public: void run() { helper(); } }; void helper() {}",
        );
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_csharp_types_inheritance_implementations_and_calls() {
        let file = parsed(
            "App.cs",
            "interface IRun { void Run(); } class Base {} class App : Base, IRun { public void Run() { Helper(); } static void Helper() {} }",
        );
        assert!(has_node(&file, "interface", "IRun"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "Run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "inherits", "IRun"));
        assert!(has_edge(&file, "calls", "Helper"));
    }

    #[test]
    fn extracts_php_types_implementations_and_calls() {
        let file = parsed(
            "app.php",
            "<?php interface Run { public function run(); } class App implements Run { public function run() { helper(); } } function helper() {}",
        );
        assert!(has_node(&file, "interface", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_ruby_modules_inheritance_requires_and_calls() {
        let file = parsed(
            "app.rb",
            "require_relative 'helper'\nmodule Core; end\nclass Base; end\nclass App < Base\n  def run\n    helper()\n  end\nend\ndef helper; end\n",
        );
        assert!(has_node(&file, "module", "Core"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "imports", "helper.rb"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_swift_protocols_inheritance_methods_and_calls() {
        let file = parsed(
            "App.swift",
            "protocol Run { func run() }\nclass Base {}\nclass App: Base, Run { func run() { helper() } }\nfunc helper() {}\n",
        );
        assert!(has_node(&file, "interface", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "inherits", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_kotlin_interfaces_inheritance_methods_and_calls() {
        let file = parsed(
            "App.kt",
            "interface Run {\n    fun run(): Unit\n}\nopen class Base {}\nclass App : Base(), Run {\n    override fun run() { helper() }\n}\nfun helper() {}\n",
        );
        assert!(has_node(&file, "interface", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "inherits", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_scala_traits_inheritance_methods_and_calls() {
        let file = parsed(
            "App.scala",
            "trait Run { def run(): Unit }\nclass Base\nclass App extends Base with Run { def run(): Unit = helper() }\ndef helper(): Unit = ()\n",
        );
        assert!(has_node(&file, "trait", "Run"));
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "inherits", "Run"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_dart_types_methods_inheritance_and_calls() {
        let file = parsed(
            "app.dart",
            "abstract class Run { void run(); }\nmixin Track {}\nclass Base {}\nclass App extends Base with Track implements Run { void run() { helper(); } }\nvoid helper() {}\n",
        );
        assert!(has_node(&file, "class", "App"));
        assert!(has_node(&file, "method", "run"));
        assert!(has_node(&file, "function", "helper"));
        assert!(has_edge(&file, "inherits", "Base"));
        assert!(has_edge(&file, "inherits", "Run"));
        assert!(has_edge(&file, "inherits", "Track"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_lua_methods_requires_and_calls() {
        let file = parsed(
            "app.lua",
            "local helper_module = require(\"helper.lua\")\nfunction App.run() helper() end\nfunction helper() end\n",
        );
        assert!(has_node(&file, "method", "run"));
        assert!(has_node(&file, "function", "helper"));
        assert!(has_edge(&file, "imports", "helper.lua"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_luau_types_requires_and_calls() {
        let file = parsed(
            "app.luau",
            "export type Result = string\nlocal helper_module = require(\"helper\")\nfunction run(): () helper() end\nfunction helper(): () end\n",
        );
        assert!(has_node(&file, "type_alias", "Result"));
        assert!(has_node(&file, "function", "run"));
        assert!(has_edge(&file, "imports", "helper.luau"));
        assert!(has_edge(&file, "calls", "helper"));
    }

    #[test]
    fn extracts_svelte_routes_components_and_typescript_bridges() {
        let file = parsed(
            "src/routes/dashboard/+page.svelte",
            r#"<script lang="ts">
import Card from "$lib/Card.svelte";
function track() {}
function handleClick() { track(); }
</script>
<Card on:click={handleClick} />
"#,
        );
        assert!(has_node(&file, "component", "+page"));
        assert!(has_node(&file, "route", "/dashboard"));
        assert!(has_node(&file, "function", "handleClick"));
        assert!(has_edge(&file, "imports", "Card"));
        assert!(has_edge(&file, "renders", "Card"));
        assert!(has_edge(&file, "binds", "handleClick"));
        assert!(has_edge(&file, "calls", "track"));
        assert!(has_local_edge(&file, "routes_to", "/dashboard", "+page"));
    }

    #[test]
    fn extracts_vue_routes_components_and_script_setup_bridges() {
        let file = parsed(
            "src/pages/users/[id].vue",
            r#"<script setup lang="ts">
import ProfileCard from "../ProfileCard.vue";
const save = () => submit();
function submit() {}
</script>
<template><ProfileCard @click="save" /></template>
"#,
        );
        assert!(has_node(&file, "component", "[id]"));
        assert!(has_node(&file, "route", "/users/:id"));
        assert!(has_node(&file, "function", "save"));
        assert!(has_edge(&file, "renders", "ProfileCard"));
        assert!(has_edge(&file, "binds", "save"));
        assert!(has_edge(&file, "calls", "submit"));
        assert!(has_local_edge(&file, "routes_to", "/users/:id", "[id]"));
    }

    #[test]
    fn extracts_astro_routes_components_and_frontmatter_bridges() {
        let file = parsed(
            "src/pages/blog/[slug].astro",
            r#"---
import Layout from "../../layouts/Layout.astro";
function load() { fetchPost(); }
function fetchPost() {}
---
<Layout onClick={load}><article>Post</article></Layout>
"#,
        );
        assert!(has_node(&file, "component", "[slug]"));
        assert!(has_node(&file, "route", "/blog/:slug"));
        assert!(has_node(&file, "function", "load"));
        assert!(has_edge(&file, "imports", "Layout"));
        assert!(has_edge(&file, "renders", "Layout"));
        assert!(has_edge(&file, "binds", "load"));
        assert!(has_edge(&file, "calls", "fetchPost"));
        assert!(has_local_edge(&file, "routes_to", "/blog/:slug", "[slug]"));
    }

    #[test]
    fn extracts_liquid_render_and_output_bridges() {
        let file = parsed(
            "snippets/card.liquid",
            "<article>{{ product.title }}</article>{% render 'badge' %}",
        );
        assert!(has_node(&file, "component", "card"));
        assert!(has_edge(&file, "binds", "product"));
        assert!(has_edge(&file, "renders", "badge"));
    }

    #[test]
    fn language_registry_is_the_discovery_contract() {
        let supported = adapters::supported_languages();
        assert_eq!(supported.len(), 21);
        for path in [
            "lib.rs",
            "app.tsx",
            "app.jsx",
            "app.py",
            "app.go",
            "App.java",
            "app.c",
            "app.cpp",
            "App.cs",
            "app.php",
            "app.rb",
            "App.swift",
            "App.kt",
            "App.scala",
            "app.dart",
            "app.lua",
            "app.luau",
            "Page.svelte",
            "Page.vue",
            "Page.astro",
            "page.liquid",
        ] {
            assert!(adapters::for_path(Path::new(path)).is_some(), "{path}");
        }
        assert!(adapters::for_path(Path::new("README.md")).is_none());
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

    #[test]
    fn refresh_resolves_edges_across_language_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spectra-polyglot-index-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("app.py"),
            "from helpers import helper\ndef run(): helper()\n",
        )
        .unwrap();
        fs::write(root.join("helpers.py"), "def helper(): pass\n").unwrap();
        fs::write(root.join("ignored.txt"), "def not_source(): pass\n").unwrap();

        let (index, report) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(report.files, 2);
        let run = index
            .graph
            .nodes
            .iter()
            .find(|node| index.graph.atom(node.label) == "run")
            .unwrap()
            .id;
        let helper = index
            .graph
            .nodes
            .iter()
            .find(|node| index.graph.atom(node.label) == "helper")
            .unwrap()
            .id;
        assert!(index.graph.edges.iter().any(|edge| {
            edge.source == run && edge.target == helper && index.graph.atom(edge.kind) == "calls"
        }));
        assert!(
            index
                .graph
                .edges
                .iter()
                .any(|edge| { edge.target == helper && index.graph.atom(edge.kind) == "imports" })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_resolves_header_imports_and_infers_inheritance_kinds() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spectra-semantic-edge-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("helper.h"), "void helper(void);\n").unwrap();
        fs::write(
            root.join("app.c"),
            "#include \"helper.h\"\nvoid run(void) { helper(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("App.cs"),
            "interface IRun { void Run(); } class Base {} class App : Base, IRun { public void Run() {} }",
        )
        .unwrap();

        let (index, report) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(report.files, 3);
        let find = |kind: &str, label: &str| {
            index
                .graph
                .nodes
                .iter()
                .find(|node| {
                    index.graph.kind(node.id) == kind && index.graph.atom(node.label) == label
                })
                .unwrap()
                .id
        };
        let app = find("class", "App");
        let base = find("class", "Base");
        let interface = find("interface", "IRun");
        let header = find("file", "helper.h");
        assert!(index.graph.edges.iter().any(|edge| {
            edge.source == app && edge.target == base && index.graph.atom(edge.kind) == "extends"
        }));
        assert!(index.graph.edges.iter().any(|edge| {
            edge.source == app
                && edge.target == interface
                && index.graph.atom(edge.kind) == "implements"
        }));
        assert!(
            index
                .graph
                .edges
                .iter()
                .any(|edge| { edge.target == header && index.graph.atom(edge.kind) == "imports" })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_resolves_web_routes_components_and_embedded_handlers() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "spectra-web-bridge-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("src/routes")).unwrap();
        fs::create_dir_all(root.join("src/lib")).unwrap();
        fs::write(
            root.join("src/routes/+page.svelte"),
            r#"<script lang="ts">
import Card from "$lib/Card.svelte";
function handleClick() {}
</script>
<Card on:click={handleClick} />
"#,
        )
        .unwrap();
        fs::write(
            root.join("src/lib/Card.svelte"),
            "<article><slot /></article>\n",
        )
        .unwrap();

        let (index, report) = CodeIndex::refresh(&root).unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(index.version, 3);
        let find = |kind: &str, label: &str| {
            index
                .graph
                .nodes
                .iter()
                .find(|node| {
                    index.graph.kind(node.id) == kind && index.graph.atom(node.label) == label
                })
                .unwrap()
                .id
        };
        let route = find("route", "/");
        let page = find("component", "+page");
        let card = find("component", "Card");
        let handler = find("function", "handleClick");
        for (source, target, kind) in [
            (route, page, "routes_to"),
            (page, card, "renders"),
            (page, handler, "binds"),
            (page, handler, "contains"),
        ] {
            assert!(index.graph.edges.iter().any(|edge| {
                edge.source == source
                    && edge.target == target
                    && index.graph.atom(edge.kind) == kind
            }));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
