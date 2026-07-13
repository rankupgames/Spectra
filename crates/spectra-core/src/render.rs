use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::{CodeIndex, Error, Result, Selection, graph::NodeId};

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 1536,
            height: 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MapArtifact {
    pub png_path: PathBuf,
    pub svg_path: PathBuf,
    pub anchors: Vec<(String, SourceAnchor)>,
    pub truncated: bool,
    pub node_count: usize,
    pub index_version: u32,
}

#[derive(Clone, Debug)]
pub struct SourceAnchor {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

pub fn render_map(
    index: &CodeIndex,
    selection: &Selection,
    query: &str,
    output_dir: &Path,
    options: RenderOptions,
) -> Result<MapArtifact> {
    fs::create_dir_all(output_dir)?;
    let stem = format!("topology-{:016x}", stable_hash(query.as_bytes()));
    let svg_path = output_dir.join(format!("{stem}.svg"));
    let png_path = output_dir.join(format!("{stem}.png"));
    let svg = render_svg(index, selection, query, options);
    fs::write(&svg_path, svg.as_bytes())?;
    rasterize(&svg, &png_path, options)?;

    let anchors = selection
        .anchors
        .iter()
        .enumerate()
        .filter_map(|(index_number, id)| {
            index.spans.get(id).map(|span| {
                (
                    format!("N{}", index_number + 1),
                    SourceAnchor {
                        path: span.path.clone(),
                        start_line: span.start_line,
                        end_line: span.end_line,
                    },
                )
            })
        })
        .collect();
    Ok(MapArtifact {
        png_path,
        svg_path,
        anchors,
        truncated: selection.truncated,
        node_count: selection.nodes.len(),
        index_version: index.version,
    })
}

pub fn render_svg(
    index: &CodeIndex,
    selection: &Selection,
    query: &str,
    options: RenderOptions,
) -> String {
    let positions = layout(index, selection, options);
    let visible: BTreeSet<_> = selection.nodes.iter().copied().collect();
    let anchors: BTreeMap<_, _> = selection
        .anchors
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, format!("N{}", i + 1)))
        .collect();
    let mut out = String::new();
    out.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}">"#,
        options.width, options.height, options.width, options.height
    ));
    out.push_str(r##"<defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="#64748b"/></marker></defs>"##);
    out.push_str(r##"<rect width="100%" height="100%" fill="#07111f"/><style>text{font-family:monospace;fill:#e5edf7}.sub{fill:#91a4bb;font-size:12px}.title{font-size:22px;font-weight:bold}</style>"##);
    out.push_str(&format!(
        r#"<text class="title" x="32" y="38">Spectra · {}</text>"#,
        escape(&truncate(query, 90))
    ));

    for edge in &index.graph.edges {
        if !visible.contains(&edge.source) || !visible.contains(&edge.target) {
            continue;
        }
        let (Some(&(x1, y1)), Some(&(x2, y2))) =
            (positions.get(&edge.source), positions.get(&edge.target))
        else {
            continue;
        };
        let edge_kind = index.graph.atom(edge.kind);
        let uncertain = edge_kind.contains("uncertain");
        let containment = edge_kind == "contains";
        out.push_str(&format!(r##"<path d="M{},{} C{},{} {},{} {},{}" fill="none" stroke="{}" stroke-width="{}" {} marker-end="url(#arrow)" opacity="{}"/>"##,
            x1 + 106, y1 + 27, x1 + 155, y1 + 27, x2 - 48, y2 + 27, x2, y2 + 27,
            if uncertain { "#f59e0b" } else { "#64748b" }, if containment { 1 } else { 2 },
            if uncertain { "stroke-dasharray=\"7 6\"" } else { "" }, if containment { "0.30" } else { "0.68" }));
    }

    for id in &selection.nodes {
        let &(x, y) = positions.get(id).unwrap_or(&(32, 80));
        let kind = index.graph.kind(*id);
        let color = node_color(kind);
        let is_anchor = anchors.contains_key(id);
        let radius = match kind {
            "trait" | "impl" => 25,
            "function" | "method" | "kernel" => 12,
            "file" | "module" => 4,
            _ => 2,
        };
        out.push_str(&format!(r##"<rect x="{x}" y="{y}" width="212" height="54" rx="{radius}" fill="#102033" stroke="{color}" stroke-width="{}"/>"##,
            if is_anchor { 4 } else { 2 }));
        if let Some(anchor) = anchors.get(id) {
            out.push_str(&format!(r##"<circle cx="{}" cy="{}" r="13" fill="{color}"/><text x="{}" y="{}" text-anchor="middle" font-size="11" font-weight="bold" fill="#07111f">{anchor}</text>"##, x + 196, y + 8, x + 196, y + 12));
        }
        out.push_str(&format!(
            r#"<text x="{}" y="{}" font-size="14" font-weight="bold">{}</text>"#,
            x + 12,
            y + 22,
            escape(&truncate(index.graph.label(*id), 25))
        ));
        let span = index.spans.get(id);
        let subtitle = span
            .map(|span| format!("{} · {}:{}", kind, short_path(&span.path), span.start_line))
            .unwrap_or_else(|| kind.into());
        out.push_str(&format!(
            r#"<text class="sub" x="{}" y="{}">{}</text>"#,
            x + 12,
            y + 43,
            escape(&truncate(&subtitle, 31))
        ));
    }
    out.push_str(r##"<g transform="translate(32,970)"><text class="sub">solid: confirmed · dashed amber: runtime/ambiguous boundary · thick border: query anchor</text></g>"##);
    out.push_str("</svg>");
    out
}

fn layout(
    index: &CodeIndex,
    selection: &Selection,
    options: RenderOptions,
) -> BTreeMap<NodeId, (i32, i32)> {
    let component = cycle_components(index, selection);
    let mut ordered = selection.nodes.clone();
    ordered.sort_by_key(|node| {
        (
            selection.distances.get(node).copied().unwrap_or(u32::MAX),
            component.get(node).copied().unwrap_or(u32::MAX),
            index
                .spans
                .get(node)
                .map(|span| span.path.as_str())
                .unwrap_or(""),
            *node,
        )
    });
    let max_columns = 6_usize;
    let columns = ordered.len().clamp(1, max_columns);
    let rows = ordered.len().div_ceil(columns).max(1);
    let columns = ordered.len().div_ceil(rows).max(1);
    let column_width = ((options.width as i32 - 80) / columns as i32).max(226);
    let available = options.height as i32 - 145;
    let row_height = (available / rows as i32).clamp(60, 100);
    let mut result = BTreeMap::new();
    for (index_number, node) in ordered.into_iter().enumerate() {
        let column = index_number / rows;
        let row = index_number % rows;
        result.insert(
            node,
            (
                32 + column as i32 * column_width,
                64 + row as i32 * row_height,
            ),
        );
    }
    result
}

/// Tarjan SCC IDs keep cycle members adjacent in the deterministic layout.
fn cycle_components(index: &CodeIndex, selection: &Selection) -> BTreeMap<NodeId, u32> {
    struct Tarjan<'a> {
        index: &'a CodeIndex,
        visible: BTreeSet<NodeId>,
        cursor: u32,
        stack: Vec<NodeId>,
        on_stack: BTreeSet<NodeId>,
        indices: BTreeMap<NodeId, u32>,
        low: BTreeMap<NodeId, u32>,
        components: BTreeMap<NodeId, u32>,
        next_component: u32,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, node: NodeId) {
            let index_number = self.cursor;
            self.cursor += 1;
            self.indices.insert(node, index_number);
            self.low.insert(node, index_number);
            self.stack.push(node);
            self.on_stack.insert(node);
            let targets: Vec<_> = self
                .index
                .graph
                .outgoing(node)
                .iter()
                .filter_map(|edge| {
                    self.index
                        .graph
                        .edge(*edge)
                        .map(|edge| edge.target)
                        .filter(|target| self.visible.contains(target))
                })
                .collect();
            for target in targets {
                if !self.indices.contains_key(&target) {
                    self.visit(target);
                    let target_low = self.low[&target];
                    self.low
                        .entry(node)
                        .and_modify(|value| *value = (*value).min(target_low));
                } else if self.on_stack.contains(&target) {
                    let target_index = self.indices[&target];
                    self.low
                        .entry(node)
                        .and_modify(|value| *value = (*value).min(target_index));
                }
            }
            if self.low[&node] == self.indices[&node] {
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(&member);
                    self.components.insert(member, self.next_component);
                    if member == node {
                        break;
                    }
                }
                self.next_component += 1;
            }
        }
    }
    let mut tarjan = Tarjan {
        index,
        visible: selection.nodes.iter().copied().collect(),
        cursor: 0,
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        indices: BTreeMap::new(),
        low: BTreeMap::new(),
        components: BTreeMap::new(),
        next_component: 0,
    };
    for node in &selection.nodes {
        if !tarjan.indices.contains_key(node) {
            tarjan.visit(*node);
        }
    }
    tarjan.components
}

fn rasterize(svg: &str, path: &Path, options: RenderOptions) -> Result<()> {
    let mut parser_options = resvg::usvg::Options::default();
    parser_options.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_str(svg, &parser_options)
        .map_err(|error| Error::Render(error.to_string()))?;
    let mut pixmap = tiny_skia::Pixmap::new(options.width, options.height)
        .ok_or_else(|| Error::Render("unable to allocate PNG surface".into()))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap
        .save_png(path)
        .map_err(|error| Error::Render(error.to_string()))
}

fn node_color(kind: &str) -> &'static str {
    match kind {
        "file" | "module" => "#38bdf8",
        "trait" | "impl" => "#c084fc",
        "struct" | "enum" => "#34d399",
        "function" | "method" => "#fbbf24",
        "boundary" => "#f97316",
        _ => "#94a3b8",
    }
}

fn short_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    }
}
fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceSpan, graph::PackedGraph};

    #[test]
    fn svg_is_deterministic_and_contains_no_source_body() {
        let mut graph = PackedGraph::default();
        let node = graph.add_node("function", "launch");
        let index = CodeIndex {
            graph,
            spans: [(
                node,
                SourceSpan {
                    path: "src/lib.rs".into(),
                    start_line: 4,
                    end_line: 9,
                },
            )]
            .into(),
            qualified_names: [(node, "launch".into())].into(),
            version: 1,
        };
        let selection = Selection {
            nodes: vec![node],
            anchors: vec![node],
            distances: [(node, 0)].into(),
            truncated: false,
        };
        let first = render_svg(&index, &selection, "launch", RenderOptions::default());
        let second = render_svg(&index, &selection, "launch", RenderOptions::default());
        assert_eq!(first, second);
        assert!(first.contains("N1"));
        assert!(!first.contains("fn launch"));
    }

    #[test]
    fn png_has_the_requested_bounded_dimensions() {
        let mut graph = PackedGraph::default();
        let node = graph.add_node("function", "launch");
        let index = CodeIndex {
            graph,
            spans: [(
                node,
                SourceSpan {
                    path: "src/lib.rs".into(),
                    start_line: 4,
                    end_line: 9,
                },
            )]
            .into(),
            qualified_names: [(node, "launch".into())].into(),
            version: 1,
        };
        let selection = Selection {
            nodes: vec![node],
            anchors: vec![node],
            distances: [(node, 0)].into(),
            truncated: false,
        };
        let output =
            std::env::temp_dir().join(format!("spectra-render-test-{}", std::process::id()));
        let artifact = render_map(
            &index,
            &selection,
            "launch",
            &output,
            RenderOptions::default(),
        )
        .unwrap();
        let bytes = fs::read(artifact.png_path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1536);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 1024);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn cycles_share_a_component() {
        let mut graph = PackedGraph::default();
        let a = graph.add_node("function", "a");
        let b = graph.add_node("function", "b");
        graph.add_edge(a, b, "calls").unwrap();
        graph.add_edge(b, a, "calls").unwrap();
        let index = CodeIndex {
            graph,
            spans: BTreeMap::new(),
            qualified_names: BTreeMap::new(),
            version: 1,
        };
        let selection = Selection {
            nodes: vec![a, b],
            anchors: vec![a],
            distances: [(a, 0), (b, 1)].into(),
            truncated: false,
        };
        let components = cycle_components(&index, &selection);
        assert_eq!(components[&a], components[&b]);
    }
}
