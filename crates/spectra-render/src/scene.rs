use std::collections::{BTreeMap, BTreeSet};

use spectra_core::{
    CodeIndex, Selection,
    graph::{NodeId, PackedGraph},
};

use crate::{
    Error, MAX_RENDER_EDGES, MAX_RENDER_NODES, MapRelation, RenderOptions, Result,
    StableNodeMetadata, StableVisualAnchor,
};

pub(crate) const MAX_RELATIONS: usize = 16;
pub(crate) const MAX_ADJACENCY_SCANS: usize = MAX_RENDER_EDGES * 16;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_STABLE_REF_BYTES: usize = 2048;
const MAX_OBJECT_HASH_BYTES: usize = 512;
pub(crate) const MAX_TITLE_BYTES: usize = 1024;
const MAX_SUBTITLE_BYTES: usize = 2048;
const MAX_KIND_BYTES: usize = 256;
pub(crate) const CARD_HEIGHT: i32 = 54;
const MIN_CARD_WIDTH: i32 = 92;
const MAX_CARD_WIDTH: i32 = 212;
const MAX_COLUMNS: usize = 6;
const COLUMN_GAP: i32 = 12;
const LAYOUT_LEFT: i32 = 32;
const LAYOUT_RIGHT: i32 = 32;
const LAYOUT_TOP: i32 = 64;
const FOOTER_RESERVE: i32 = 40;

#[derive(Clone, Copy)]
pub(crate) struct Ink {
    pub(crate) hex: &'static str,
    pub(crate) rgba: [u8; 4],
}

pub(crate) const BACKGROUND: Ink = Ink {
    hex: "#07111f",
    rgba: [7, 17, 31, 255],
};
pub(crate) const PANEL: Ink = Ink {
    hex: "#102033",
    rgba: [16, 32, 51, 255],
};
pub(crate) const TEXT: Ink = Ink {
    hex: "#e5edf7",
    rgba: [229, 237, 247, 255],
};
pub(crate) const SUBTEXT: Ink = Ink {
    hex: "#91a4bb",
    rgba: [145, 164, 187, 255],
};
pub(crate) const EDGE: Ink = Ink {
    hex: "#64748b",
    rgba: [100, 116, 139, 255],
};
pub(crate) const UNCERTAIN: Ink = Ink {
    hex: "#f59e0b",
    rgba: [245, 158, 11, 255],
};

pub(crate) struct Scene {
    pub(crate) query: String,
    pub(crate) options: RenderOptions,
    pub(crate) nodes: Vec<SceneNode>,
    pub(crate) edges: Vec<SceneEdge>,
    pub(crate) anchors: Vec<StableVisualAnchor>,
    pub(crate) relations: Vec<MapRelation>,
    pub(crate) truncated: bool,
}

pub(crate) struct SceneNode {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) stable_ref: String,
    pub(crate) object_hash: Option<String>,
    pub(crate) color: Ink,
    pub(crate) anchor: Option<String>,
}

pub(crate) struct SceneEdge {
    pub(crate) x1: i32,
    pub(crate) y1: i32,
    pub(crate) x2: i32,
    pub(crate) y2: i32,
    pub(crate) uncertain: bool,
    pub(crate) containment: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedEdge {
    source: NodeId,
    target: NodeId,
    kind: String,
}

struct Layout {
    positions: BTreeMap<NodeId, (i32, i32)>,
    card_width: i32,
    ordered_nodes: Vec<NodeId>,
}

pub(crate) fn build_scene(
    graph: &PackedGraph,
    selection: &Selection,
    query: &str,
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
    options: RenderOptions,
) -> Result<Scene> {
    validate_bounded_text("query", query, MAX_QUERY_BYTES, true)?;
    if selection.nodes.is_empty() {
        return Err(Error::Render(
            "selection must contain at least one node".into(),
        ));
    }
    if selection.nodes.len() > MAX_RENDER_NODES {
        return Err(Error::Render(format!(
            "selection exceeds the {MAX_RENDER_NODES}-node render ceiling"
        )));
    }

    let visible = selection.nodes.iter().copied().collect::<BTreeSet<_>>();
    if visible.len() != selection.nodes.len() {
        return Err(Error::Render("selection contains duplicate nodes".into()));
    }
    let mut stable_refs = BTreeSet::new();
    for node in &selection.nodes {
        if graph.node(*node).is_none() {
            return Err(Error::Render(format!(
                "selection references missing node {}",
                node.0
            )));
        }
        let metadata = metadata.get(node).ok_or_else(|| {
            Error::Render(format!("selected node {} has no stable metadata", node.0))
        })?;
        validate_bounded_text(
            "stable reference",
            &metadata.stable_ref,
            MAX_STABLE_REF_BYTES,
            false,
        )?;
        validate_bounded_text("title", &metadata.title, MAX_TITLE_BYTES, false)?;
        validate_bounded_text("subtitle", &metadata.subtitle, MAX_SUBTITLE_BYTES, true)?;
        if let Some(hash) = &metadata.object_hash {
            validate_bounded_text("object hash", hash, MAX_OBJECT_HASH_BYTES, false)?;
        }
        validate_bounded_text("node kind", graph.kind(*node), MAX_KIND_BYTES, false)?;
        if !stable_refs.insert(metadata.stable_ref.as_str()) {
            return Err(Error::Render(format!(
                "duplicate stable reference {}",
                metadata.stable_ref
            )));
        }
    }
    let anchor_nodes = selection.anchors.iter().copied().collect::<BTreeSet<_>>();
    if anchor_nodes.len() != selection.anchors.len()
        || anchor_nodes.iter().any(|node| !visible.contains(node))
    {
        return Err(Error::Render(
            "anchors must be unique members of the selected node set".into(),
        ));
    }

    let (selected_edges, edge_truncated) = selected_edges(graph, &visible, metadata)?;
    let layout = layout(selection, metadata, &selected_edges, options)?;
    let mut ordered_anchors = selection.anchors.clone();
    ordered_anchors.sort_by(|left, right| {
        metadata[left]
            .stable_ref
            .cmp(&metadata[right].stable_ref)
            .then_with(|| left.cmp(right))
    });
    let anchor_ids = ordered_anchors
        .iter()
        .enumerate()
        .map(|(index, node)| (*node, format!("N{}", index + 1)))
        .collect::<BTreeMap<_, _>>();
    let anchors = ordered_anchors
        .iter()
        .map(|node| {
            let stable = &metadata[node];
            StableVisualAnchor {
                visual_id: anchor_ids[node].clone(),
                stable_ref: stable.stable_ref.clone(),
                object_hash: stable.object_hash.clone(),
                kind: graph.kind(*node).to_owned(),
                title: stable.title.clone(),
                subtitle: stable.subtitle.clone(),
            }
        })
        .collect();

    let nodes = layout
        .ordered_nodes
        .iter()
        .map(|node| {
            let stable = &metadata[node];
            let (x, y) = layout.positions[node];
            SceneNode {
                x,
                y,
                width: layout.card_width,
                kind: graph.kind(*node).to_owned(),
                title: stable.title.clone(),
                subtitle: stable.subtitle.clone(),
                stable_ref: stable.stable_ref.clone(),
                object_hash: stable.object_hash.clone(),
                color: node_color(graph.kind(*node)),
                anchor: anchor_ids.get(node).cloned(),
            }
        })
        .collect::<Vec<_>>();

    let mut edges = Vec::with_capacity(selected_edges.len());
    let mut relation_keys = BTreeSet::new();
    let mut relations = Vec::new();
    let mut relation_truncated = false;
    for edge in selected_edges {
        let (Some(&(x1, y1)), Some(&(x2, y2))) = (
            layout.positions.get(&edge.source),
            layout.positions.get(&edge.target),
        ) else {
            continue;
        };
        edges.push(SceneEdge {
            x1: x1 + layout.card_width / 2,
            y1: y1 + CARD_HEIGHT / 2,
            x2,
            y2: y2 + CARD_HEIGHT / 2,
            uncertain: edge.kind.contains("uncertain"),
            containment: edge.kind == "contains",
        });

        let (Some(source), Some(target)) =
            (anchor_ids.get(&edge.source), anchor_ids.get(&edge.target))
        else {
            continue;
        };
        if source == target {
            continue;
        }
        let key = (source.clone(), edge.kind, target.clone());
        if relation_keys.insert(key.clone()) {
            if relations.len() < MAX_RELATIONS {
                relations.push(MapRelation {
                    source: key.0,
                    kind: key.1,
                    target: key.2,
                });
            } else {
                relation_truncated = true;
            }
        }
    }

    Ok(Scene {
        query: query.to_owned(),
        options,
        nodes,
        edges,
        anchors,
        relations,
        truncated: selection.truncated || edge_truncated || relation_truncated,
    })
}

pub(crate) fn selected_edges(
    graph: &PackedGraph,
    visible: &BTreeSet<NodeId>,
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
) -> Result<(Vec<SelectedEdge>, bool)> {
    let mut sources = visible.iter().copied().collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        metadata[left]
            .stable_ref
            .cmp(&metadata[right].stable_ref)
            .then_with(|| left.cmp(right))
    });
    let mut scanned = 0_usize;
    let mut selected = BTreeMap::new();
    for source in sources {
        let outgoing = graph.outgoing(source);
        scanned = scanned.saturating_add(outgoing.len());
        if scanned > MAX_ADJACENCY_SCANS {
            return Err(Error::Render(format!(
                "selected adjacency exceeds the {MAX_ADJACENCY_SCANS}-edge scan ceiling"
            )));
        }
        for edge_id in outgoing {
            let edge = graph.edge(*edge_id).ok_or_else(|| {
                Error::Render(format!("adjacency references missing edge {}", edge_id.0))
            })?;
            if visible.contains(&edge.target) {
                let kind = graph.atom(edge.kind);
                validate_bounded_text("edge kind", kind, MAX_KIND_BYTES, false)?;
                let key = (
                    metadata[&edge.source].stable_ref.clone(),
                    kind.to_owned(),
                    metadata[&edge.target].stable_ref.clone(),
                );
                selected.entry(key).or_insert_with(|| SelectedEdge {
                    source: edge.source,
                    target: edge.target,
                    kind: kind.to_owned(),
                });
            }
        }
    }
    let truncated = selected.len() > MAX_RENDER_EDGES;
    Ok((
        selected.into_values().take(MAX_RENDER_EDGES).collect(),
        truncated,
    ))
}

pub(crate) fn code_index_metadata(
    index: &CodeIndex,
    selection: &Selection,
) -> BTreeMap<NodeId, StableNodeMetadata> {
    selection
        .nodes
        .iter()
        .map(|node| {
            let kind = index.graph.kind(*node);
            let label = index.graph.label(*node);
            let qualified = index
                .qualified_names
                .get(node)
                .map(String::as_str)
                .unwrap_or(label);
            let (stable_ref, subtitle) = if let Some(span) = index.spans.get(node) {
                (
                    format!(
                        "code:v{}:{}:{}:{}-{}:{}",
                        index.version, kind, span.path, span.start_line, span.end_line, qualified
                    ),
                    format!("{} - {}:{}", kind, short_path(&span.path), span.start_line),
                )
            } else {
                (
                    format!("code:v{}:{}:{}:{}", index.version, kind, node.0, qualified),
                    kind.to_owned(),
                )
            };
            (
                *node,
                StableNodeMetadata {
                    stable_ref,
                    object_hash: None,
                    title: label.to_owned(),
                    subtitle,
                },
            )
        })
        .collect()
}

fn layout(
    selection: &Selection,
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
    edges: &[SelectedEdge],
    options: RenderOptions,
) -> Result<Layout> {
    let component = cycle_components(selection, edges, metadata);
    let mut ordered = selection.nodes.clone();
    ordered.sort_by(|left, right| {
        selection
            .distances
            .get(left)
            .copied()
            .unwrap_or(u32::MAX)
            .cmp(&selection.distances.get(right).copied().unwrap_or(u32::MAX))
            .then_with(|| {
                component
                    .get(left)
                    .map(String::as_str)
                    .unwrap_or("")
                    .cmp(component.get(right).map(String::as_str).unwrap_or(""))
            })
            .then_with(|| metadata[left].stable_ref.cmp(&metadata[right].stable_ref))
            .then_with(|| left.cmp(right))
    });
    let horizontal = options.width as i32 - LAYOUT_LEFT - LAYOUT_RIGHT;
    let max_columns = (((horizontal + COLUMN_GAP) / (MIN_CARD_WIDTH + COLUMN_GAP)) as usize)
        .clamp(1, MAX_COLUMNS);
    let columns = ordered.len().clamp(1, max_columns);
    let rows = ordered.len().div_ceil(columns).max(1);
    let vertical = options.height as i32 - LAYOUT_TOP - FOOTER_RESERVE;
    if rows as i32 * CARD_HEIGHT > vertical {
        return Err(Error::Render(format!(
            "{} selected nodes do not fit within {}x{} render dimensions",
            ordered.len(),
            options.width,
            options.height
        )));
    }
    let column_width = horizontal / columns as i32;
    let card_width = (column_width - COLUMN_GAP).clamp(MIN_CARD_WIDTH, MAX_CARD_WIDTH);
    let row_height = (vertical / rows as i32).clamp(CARD_HEIGHT, 100);
    let mut result = BTreeMap::new();
    for (index_number, node) in ordered.iter().copied().enumerate() {
        let column = index_number / rows;
        let row = index_number % rows;
        result.insert(
            node,
            (
                LAYOUT_LEFT + column as i32 * column_width,
                LAYOUT_TOP + row as i32 * row_height,
            ),
        );
    }
    Ok(Layout {
        positions: result,
        card_width,
        ordered_nodes: ordered,
    })
}

/// Stable Tarjan SCC keys keep cycle members adjacent without leaking NodeId order.
pub(crate) fn cycle_components(
    selection: &Selection,
    edges: &[SelectedEdge],
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
) -> BTreeMap<NodeId, String> {
    struct Tarjan<'a> {
        adjacency: &'a BTreeMap<NodeId, Vec<NodeId>>,
        cursor: u32,
        stack: Vec<NodeId>,
        on_stack: BTreeSet<NodeId>,
        indices: BTreeMap<NodeId, u32>,
        low: BTreeMap<NodeId, u32>,
        components: Vec<Vec<NodeId>>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, node: NodeId) {
            let index_number = self.cursor;
            self.cursor += 1;
            self.indices.insert(node, index_number);
            self.low.insert(node, index_number);
            self.stack.push(node);
            self.on_stack.insert(node);
            let targets = self.adjacency.get(&node).cloned().unwrap_or_default();
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
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(&member);
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                self.components.push(component);
            }
        }
    }
    let mut adjacency = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for edge in edges {
        adjacency.entry(edge.source).or_default().push(edge.target);
    }
    for targets in adjacency.values_mut() {
        targets.sort_by(|left, right| {
            metadata[left]
                .stable_ref
                .cmp(&metadata[right].stable_ref)
                .then_with(|| left.cmp(right))
        });
        targets.dedup();
    }
    let mut tarjan = Tarjan {
        adjacency: &adjacency,
        cursor: 0,
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        indices: BTreeMap::new(),
        low: BTreeMap::new(),
        components: Vec::new(),
    };
    let mut ordered_nodes = selection.nodes.clone();
    ordered_nodes.sort_by(|left, right| {
        metadata[left]
            .stable_ref
            .cmp(&metadata[right].stable_ref)
            .then_with(|| left.cmp(right))
    });
    for node in ordered_nodes {
        if !tarjan.indices.contains_key(&node) {
            tarjan.visit(node);
        }
    }
    let mut result = BTreeMap::new();
    for component in tarjan.components {
        let key = component
            .iter()
            .map(|node| metadata[node].stable_ref.as_str())
            .min()
            .unwrap_or_default()
            .to_owned();
        for member in component {
            result.insert(member, key.clone());
        }
    }
    result
}

fn node_color(kind: &str) -> Ink {
    match kind {
        "file" | "module" => Ink {
            hex: "#38bdf8",
            rgba: [56, 189, 248, 255],
        },
        "trait" | "impl" => Ink {
            hex: "#c084fc",
            rgba: [192, 132, 252, 255],
        },
        "struct" | "enum" => Ink {
            hex: "#34d399",
            rgba: [52, 211, 153, 255],
        },
        "function" | "method" | "kernel" => Ink {
            hex: "#fbbf24",
            rgba: [251, 191, 36, 255],
        },
        "boundary" => Ink {
            hex: "#f97316",
            rgba: [249, 115, 22, 255],
        },
        _ => Ink {
            hex: "#94a3b8",
            rgba: [148, 163, 184, 255],
        },
    }
}

fn short_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn validate_bounded_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<()> {
    if !allow_empty && value.trim().is_empty() {
        return Err(Error::Render(format!("{field} must be non-empty")));
    }
    if value.len() > max_bytes {
        return Err(Error::Render(format!(
            "{field} must be no more than {max_bytes} bytes"
        )));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(Error::Render(format!(
            "{field} contains an unsupported control character"
        )));
    }
    Ok(())
}

pub(crate) fn truncate(value: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if value.chars().count() <= max {
        value.into()
    } else {
        format!("{}?", value.chars().take(max - 1).collect::<String>())
    }
}
