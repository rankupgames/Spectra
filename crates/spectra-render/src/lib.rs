use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use spectra_core::{
    CodeIndex, LedgerEventKind, LedgerStore, Selection, SelectionOptions,
    graph::{NodeId, PackedGraph},
    ledger, select_subgraph, sync_project,
};

mod glyphs;
mod png;
mod raster;
mod scene;
mod svg;
#[cfg(feature = "svg-raster-compat")]
mod svg_raster_compat;

use raster::render_scene_png;
use scene::{build_scene, code_index_metadata, truncate};
use svg::{error_svg, render_scene_svg};

pub const MAX_RENDER_NODES: usize = 96;
pub const MAX_RENDER_EDGES: usize = 384;
pub const MIN_RENDER_WIDTH: u32 = 320;
pub const MIN_RENDER_HEIGHT: u32 = 240;
pub const MAX_RENDER_WIDTH: u32 = 4096;
pub const MAX_RENDER_HEIGHT: u32 = 4096;
/// Published Cargo package identity for immutable render-policy manifests.
pub const RENDERER_PACKAGE_NAME: &str = env!("CARGO_PKG_NAME");
/// Published crate semver for immutable render-policy manifests.
pub const RENDERER_CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version of the deterministic scene compiler and direct PNG backend.
///
/// This revision changes only when the bytes or stable visual semantics emitted
/// for identical verified inputs may change. It deliberately excludes Git state.
pub const RENDERER_ALGORITHM_REVISION: &str = "scene-v2-direct-png-v10";
/// Version of the optional canonical SVG-to-PNG compatibility backend.
#[cfg(feature = "svg-raster-compat")]
pub const SVG_RASTER_COMPAT_ALGORITHM_REVISION: &str = "canonical-svg-resvg-0.47-v11";

/// Raster backend applied after the deterministic canonical Scene is built.
///
/// The default direct backend does not enable the optional `resvg` dependency.
/// The compatibility variant exists only when `svg-raster-compat` is selected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RasterBackend {
    #[default]
    Direct,
    #[cfg(feature = "svg-raster-compat")]
    SvgCompat,
}

impl RasterBackend {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            #[cfg(feature = "svg-raster-compat")]
            Self::SvgCompat => "svg-compat",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Core(spectra_core::Error),
    Io(io::Error),
    Render(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Render(message) => write!(formatter, "render error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<spectra_core::Error> for Error {
    fn from(error: spectra_core::Error) -> Self {
        Self::Core(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Refresh an index and render a query-focused topology in one operation.
pub fn map_project(
    project: &Path,
    query: &str,
    max_nodes: usize,
    output_dir: &Path,
) -> Result<MapArtifact> {
    map_project_with_backend(project, query, max_nodes, output_dir, RasterBackend::Direct)
}

pub fn map_project_with_backend(
    project: &Path,
    query: &str,
    max_nodes: usize,
    output_dir: &Path,
    raster_backend: RasterBackend,
) -> Result<MapArtifact> {
    let (index, _) = sync_project(project)?;
    let selection = select_subgraph(
        &index,
        query,
        SelectionOptions {
            max_nodes: max_nodes.clamp(1, MAX_RENDER_NODES),
        },
    );
    let artifact = render_map_with_backend(
        &index,
        &selection,
        query,
        output_dir,
        RenderOptions::default(),
        raster_backend,
    )?;
    let map_id = artifact
        .png_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("topology")
        .to_owned();
    LedgerStore::transaction(project, |ledger| {
        ledger.append(LedgerEventKind::MapRendered {
            map_id,
            query: ledger::redact_text(query),
            anchors: artifact
                .anchors
                .iter()
                .map(|(visual_id, anchor)| ledger::LedgerAnchor {
                    visual_id: visual_id.clone(),
                    path: anchor.path.clone(),
                    start_line: anchor.start_line,
                    end_line: anchor.end_line,
                })
                .collect(),
            nodes: artifact.node_count,
            truncated: artifact.truncated,
        })?;
        Ok(())
    })?;
    Ok(artifact)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

impl RenderOptions {
    fn validate(self) -> Result<Self> {
        if !(MIN_RENDER_WIDTH..=MAX_RENDER_WIDTH).contains(&self.width)
            || !(MIN_RENDER_HEIGHT..=MAX_RENDER_HEIGHT).contains(&self.height)
        {
            return Err(Error::Render(format!(
                "dimensions must be within {MIN_RENDER_WIDTH}x{MIN_RENDER_HEIGHT} and {MAX_RENDER_WIDTH}x{MAX_RENDER_HEIGHT}"
            )));
        }
        Ok(self)
    }
}

/// Domain-neutral visual identity for one selected graph node.
///
/// Stable references and object hashes belong to the caller's immutable domain.
/// They are not inferred from Spectra's snapshot-local NodeId.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableNodeMetadata {
    pub stable_ref: String,
    pub object_hash: Option<String>,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableVisualAnchor {
    pub visual_id: String,
    pub stable_ref: String,
    pub object_hash: Option<String>,
    pub kind: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedMap {
    pub png_bytes: Vec<u8>,
    pub svg_bytes: Vec<u8>,
    pub anchors: Vec<StableVisualAnchor>,
    pub relations: Vec<MapRelation>,
    pub truncated: bool,
    pub node_count: usize,
    pub edge_count: usize,
}

/// Rasterize the canonical SVG projection from a completed render for backend
/// comparison.
///
/// This API is available only with the `svg-raster-compat` feature. It disables
/// external image resolution, preserves the SVG's intrinsic bounded dimensions,
/// and does not alter the default direct Scene-to-PNG path used by `Renderer`.
#[cfg(feature = "svg-raster-compat")]
pub fn rasterize_rendered_svg_compat(rendered: &RenderedMap) -> Result<Vec<u8>> {
    svg_raster_compat::rasterize_canonical_svg(&rendered.svg_bytes)
}

/// Reusable, deterministic renderer state.
///
/// The renderer uses an embedded 3x5 vector glyph set. It performs no font
/// discovery, filesystem access, SVG parsing, or mutable global initialization.
#[derive(Clone, Debug, Default)]
pub struct Renderer;

impl Renderer {
    pub const fn new() -> Self {
        Self
    }

    /// Render a verified PackedGraph selection directly to canonical SVG and
    /// PNG bytes. Callers must rebuild graph indexes after deserialization before
    /// invoking this method; only selected adjacency is traversed at render time.
    pub fn render(
        &self,
        graph: &PackedGraph,
        selection: &Selection,
        query: &str,
        metadata: &BTreeMap<NodeId, StableNodeMetadata>,
        options: RenderOptions,
    ) -> Result<RenderedMap> {
        self.render_with_backend(
            graph,
            selection,
            query,
            metadata,
            options,
            RasterBackend::Direct,
        )
    }

    /// Render one canonical Scene through the selected raster backend.
    /// Selection, stable anchors, relations, and SVG bytes are backend-neutral.
    pub fn render_with_backend(
        &self,
        graph: &PackedGraph,
        selection: &Selection,
        query: &str,
        metadata: &BTreeMap<NodeId, StableNodeMetadata>,
        options: RenderOptions,
        raster_backend: RasterBackend,
    ) -> Result<RenderedMap> {
        let options = options.validate()?;
        let scene = build_scene(graph, selection, query, metadata, options)?;
        let svg_bytes = render_scene_svg(&scene).into_bytes();
        let png_bytes = match raster_backend {
            RasterBackend::Direct => render_scene_png(&scene)?,
            #[cfg(feature = "svg-raster-compat")]
            RasterBackend::SvgCompat => svg_raster_compat::rasterize_canonical_svg(&svg_bytes)?,
        };
        Ok(RenderedMap {
            png_bytes,
            svg_bytes,
            anchors: scene.anchors,
            relations: scene.relations,
            truncated: scene.truncated,
            node_count: scene.nodes.len(),
            edge_count: scene.edges.len(),
        })
    }
}

pub fn render_packed_graph(
    graph: &PackedGraph,
    selection: &Selection,
    query: &str,
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
    options: RenderOptions,
) -> Result<RenderedMap> {
    Renderer::new().render(graph, selection, query, metadata, options)
}

pub fn render_packed_graph_with_backend(
    graph: &PackedGraph,
    selection: &Selection,
    query: &str,
    metadata: &BTreeMap<NodeId, StableNodeMetadata>,
    options: RenderOptions,
    raster_backend: RasterBackend,
) -> Result<RenderedMap> {
    Renderer::new().render_with_backend(graph, selection, query, metadata, options, raster_backend)
}

#[derive(Clone, Debug)]
pub struct MapArtifact {
    pub png_path: PathBuf,
    pub svg_path: PathBuf,
    pub anchors: Vec<(String, SourceAnchor)>,
    pub relations: Vec<MapRelation>,
    pub truncated: bool,
    pub node_count: usize,
    pub index_version: u32,
}

#[derive(Clone, Debug)]
pub struct SourceAnchor {
    pub kind: String,
    pub qualified_name: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapRelation {
    pub source: String,
    pub kind: String,
    pub target: String,
}

impl MapArtifact {
    pub fn compact_metadata(&self) -> String {
        let mut lines = Vec::with_capacity(self.anchors.len() + self.relations.len() + 1);
        for (id, anchor) in &self.anchors {
            lines.push(format!(
                "{id}={} {} @ {}:{}-{}",
                compact_field(&anchor.kind, 24),
                compact_field(&anchor.qualified_name, 80),
                anchor.path,
                anchor.start_line,
                anchor.end_line
            ));
        }
        for relation in &self.relations {
            lines.push(format!(
                "flow {} -{}-> {}",
                relation.source,
                compact_field(&relation.kind, 32),
                relation.target
            ));
        }
        lines.push(format!(
            "nodes={} truncated={} index=v{}",
            self.node_count, self.truncated, self.index_version
        ));
        lines.join("\n")
    }
}

/// Compatibility wrapper for source-code indexes. New consumers should use
/// Renderer::render and retain their own stable domain metadata.
pub fn render_map(
    index: &CodeIndex,
    selection: &Selection,
    query: &str,
    output_dir: &Path,
    options: RenderOptions,
) -> Result<MapArtifact> {
    render_map_with_backend(
        index,
        selection,
        query,
        output_dir,
        options,
        RasterBackend::Direct,
    )
}

pub fn render_map_with_backend(
    index: &CodeIndex,
    selection: &Selection,
    query: &str,
    output_dir: &Path,
    options: RenderOptions,
    raster_backend: RasterBackend,
) -> Result<MapArtifact> {
    let metadata = code_index_metadata(index, selection);
    let rendered = Renderer::new().render_with_backend(
        &index.graph,
        selection,
        query,
        &metadata,
        options,
        raster_backend,
    )?;
    fs::create_dir_all(output_dir)?;
    let stem = format!("topology-{:016x}", stable_hash(query.as_bytes()));
    let svg_path = output_dir.join(format!("{stem}.svg"));
    let png_path = output_dir.join(format!("{stem}.png"));
    fs::write(&svg_path, &rendered.svg_bytes)?;
    fs::write(&png_path, &rendered.png_bytes)?;

    let anchors = selection
        .anchors
        .iter()
        .enumerate()
        .filter_map(|(index_number, id)| {
            let span = index.spans.get(id)?;
            let label = index.graph.label(*id);
            Some((
                format!("N{}", index_number + 1),
                SourceAnchor {
                    kind: index.graph.kind(*id).to_owned(),
                    qualified_name: index
                        .qualified_names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| label.to_owned()),
                    path: span.path.clone(),
                    start_line: span.start_line,
                    end_line: span.end_line,
                },
            ))
        })
        .collect();
    Ok(MapArtifact {
        png_path,
        svg_path,
        anchors,
        relations: rendered.relations,
        truncated: rendered.truncated,
        node_count: rendered.node_count,
        index_version: index.version,
    })
}

/// Compatibility SVG API retained for existing callers.
pub fn render_svg(
    index: &CodeIndex,
    selection: &Selection,
    query: &str,
    options: RenderOptions,
) -> String {
    let metadata = code_index_metadata(index, selection);
    match options
        .validate()
        .and_then(|options| build_scene(&index.graph, selection, query, &metadata, options))
    {
        Ok(scene) => render_scene_svg(&scene),
        Err(error) => error_svg(options, &error.to_string()),
    }
}

fn compact_field(value: &str, max: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&normalized, max)
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "svg-raster-compat")]
    use crate::scene::{BACKGROUND, SUBTEXT, TEXT};
    use crate::{
        glyphs::glyph,
        scene::{
            MAX_ADJACENCY_SCANS, MAX_RELATIONS, MAX_TITLE_BYTES, cycle_components, selected_edges,
        },
    };
    use spectra_core::SourceSpan;
    #[cfg(feature = "svg-raster-compat")]
    use std::io::Cursor;
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    static RENDER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn render_test_guard() -> MutexGuard<'static, ()> {
        RENDER_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(feature = "svg-raster-compat")]
    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = ::png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(info.bit_depth, ::png::BitDepth::Eight);
        pixels.truncate(info.buffer_size());
        let pixels = match info.color_type {
            ::png::ColorType::Rgb => pixels
                .chunks_exact(3)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
                .collect(),
            ::png::ColorType::Rgba => pixels,
            color_type => panic!("unexpected PNG color type {color_type:?}"),
        };
        (info.width, info.height, pixels)
    }

    #[cfg(feature = "svg-raster-compat")]
    fn rgba_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        pixels[offset..offset + 4].try_into().unwrap()
    }

    #[cfg(feature = "svg-raster-compat")]
    fn count_color_in_rows(
        pixels: &[u8],
        width: u32,
        rows: std::ops::Range<u32>,
        color: [u8; 4],
    ) -> usize {
        rows.flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| rgba_at(pixels, width, x, y) == color)
            .count()
    }

    #[cfg(feature = "svg-raster-compat")]
    fn first_filled_glyph_pixel(character: char, character_index: u32) -> (u32, u32) {
        for (row, bits) in glyph(character).iter().enumerate() {
            for column in 0..3 {
                if bits & (1 << (2 - column)) != 0 {
                    return (
                        32 + character_index * 12 + column * 3 + 1,
                        20 + row as u32 * 3 + 1,
                    );
                }
            }
        }
        panic!("test character must contain at least one filled glyph cell");
    }

    fn stable_metadata(nodes: &[(NodeId, &str, &str)]) -> BTreeMap<NodeId, StableNodeMetadata> {
        nodes
            .iter()
            .map(|(node, stable_ref, title)| {
                (
                    *node,
                    StableNodeMetadata {
                        stable_ref: (*stable_ref).into(),
                        object_hash: Some(format!(
                            "sha256:{:064x}",
                            stable_hash(stable_ref.as_bytes())
                        )),
                        title: (*title).into(),
                        subtitle: "immutable skill fragment".into(),
                    },
                )
            })
            .collect()
    }

    fn bounded_fixture(
        node_count: usize,
        edge_count: usize,
    ) -> (PackedGraph, Selection, BTreeMap<NodeId, StableNodeMetadata>) {
        assert!(node_count > 1);
        let mut graph = PackedGraph::default();
        let nodes = (0..node_count)
            .map(|index| graph.add_node("fragment", &format!("fragment-{index:03}")))
            .collect::<Vec<_>>();
        for edge_index in 0..edge_count {
            let source_index = edge_index % node_count;
            let offset = edge_index / node_count + 1;
            let target_index = (source_index + offset) % node_count;
            graph
                .add_edge(nodes[source_index], nodes[target_index], "related")
                .unwrap();
        }
        let metadata = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let stable_ref = format!("fragment:sha256:{index:064x}");
                (
                    *node,
                    StableNodeMetadata {
                        object_hash: Some(format!("sha256:{index:064x}")),
                        title: format!("Fragment {index:03}"),
                        subtitle: "immutable skill fragment".into(),
                        stable_ref,
                    },
                )
            })
            .collect();
        let selection = Selection {
            nodes: nodes.clone(),
            anchors: vec![nodes[0]],
            distances: nodes.iter().map(|node| (*node, 0)).collect(),
            truncated: false,
        };
        (graph, selection, metadata)
    }

    #[test]
    fn in_memory_render_is_byte_deterministic_and_preserves_stable_anchors() {
        let _guard = render_test_guard();
        let mut graph = PackedGraph::default();
        let source = graph.add_node("fragment", "route");
        let target = graph.add_node("obligation", "verify");
        graph.add_edge(source, target, "requires").unwrap();
        let selection = Selection {
            nodes: vec![source, target],
            anchors: vec![source, target],
            distances: [(source, 0), (target, 1)].into(),
            truncated: false,
        };
        let metadata = stable_metadata(&[
            (source, "fragment:sha256:source", "Route immutable skills"),
            (target, "obligation:sha256:target", "Verify route proof"),
        ]);
        let renderer = Renderer::new();
        let first = renderer
            .render(
                &graph,
                &selection,
                "route skills",
                &metadata,
                RenderOptions::default(),
            )
            .unwrap();
        let second = renderer
            .render(
                &graph,
                &selection,
                "route skills",
                &metadata,
                RenderOptions::default(),
            )
            .unwrap();
        assert_eq!(first.svg_bytes, second.svg_bytes);
        assert_eq!(first.png_bytes, second.png_bytes);
        assert_eq!(first.anchors[0].visual_id, "N1");
        assert_eq!(first.anchors[0].stable_ref, "fragment:sha256:source");
        assert_eq!(first.relations.len(), 1);
        assert_eq!(&first.png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn render_is_deterministic_across_snapshot_local_node_and_edge_order() {
        let _guard = render_test_guard();
        let mut first_graph = PackedGraph::default();
        let first_alpha = first_graph.add_node("fragment", "alpha");
        let first_beta = first_graph.add_node("fragment", "beta");
        let first_gamma = first_graph.add_node("fragment", "gamma");
        first_graph
            .add_edge(first_alpha, first_beta, "requires")
            .unwrap();
        first_graph
            .add_edge(first_beta, first_gamma, "related")
            .unwrap();
        let first_selection = Selection {
            nodes: vec![first_alpha, first_beta, first_gamma],
            anchors: vec![first_alpha, first_gamma],
            distances: [(first_alpha, 0), (first_beta, 0), (first_gamma, 0)].into(),
            truncated: false,
        };
        let first_metadata = stable_metadata(&[
            (first_alpha, "fragment:alpha", "Alpha"),
            (first_beta, "fragment:beta", "Beta"),
            (first_gamma, "fragment:gamma", "Gamma"),
        ]);

        let mut second_graph = PackedGraph::default();
        let second_gamma = second_graph.add_node("fragment", "gamma");
        let second_beta = second_graph.add_node("fragment", "beta");
        let second_alpha = second_graph.add_node("fragment", "alpha");
        second_graph
            .add_edge(second_beta, second_gamma, "related")
            .unwrap();
        second_graph
            .add_edge(second_alpha, second_beta, "requires")
            .unwrap();
        let second_selection = Selection {
            nodes: vec![second_gamma, second_alpha, second_beta],
            anchors: vec![second_gamma, second_alpha],
            distances: [(second_alpha, 0), (second_beta, 0), (second_gamma, 0)].into(),
            truncated: false,
        };
        let second_metadata = stable_metadata(&[
            (second_alpha, "fragment:alpha", "Alpha"),
            (second_beta, "fragment:beta", "Beta"),
            (second_gamma, "fragment:gamma", "Gamma"),
        ]);

        let first = render_packed_graph(
            &first_graph,
            &first_selection,
            "stable projection",
            &first_metadata,
            RenderOptions::default(),
        )
        .unwrap();
        let second = render_packed_graph(
            &second_graph,
            &second_selection,
            "stable projection",
            &second_metadata,
            RenderOptions::default(),
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn svg_compatibility_api_is_deterministic_and_contains_no_source_body() {
        let _guard = render_test_guard();
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
        assert!(first.contains("data-stable-ref"));
        assert!(!first.contains("fn launch"));
        assert!(!first.contains("font-family"));
    }

    #[test]
    fn png_compatibility_wrapper_has_requested_bounded_dimensions() {
        let _guard = render_test_guard();
        let mut graph = PackedGraph::default();
        let node = graph.add_node("function", "launch");
        let target = graph.add_node("method", "execute");
        graph.add_edge(node, target, "calls").unwrap();
        graph.add_edge(node, target, "calls").unwrap();
        graph.add_edge(target, target, "calls").unwrap();
        let index = CodeIndex {
            graph,
            spans: [
                (
                    node,
                    SourceSpan {
                        path: "src/lib.rs".into(),
                        start_line: 4,
                        end_line: 9,
                    },
                ),
                (
                    target,
                    SourceSpan {
                        path: "src/worker.rs".into(),
                        start_line: 12,
                        end_line: 18,
                    },
                ),
            ]
            .into(),
            qualified_names: [(node, "launch".into()), (target, "Worker::execute".into())].into(),
            version: 1,
        };
        let selection = Selection {
            nodes: vec![node, target],
            anchors: vec![node, target],
            distances: [(node, 0), (target, 1)].into(),
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
        let bytes = fs::read(&artifact.png_path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1536);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 1024);
        assert!(
            artifact
                .compact_metadata()
                .contains("N2=method Worker::execute @ src/worker.rs:12-18")
        );
        assert_eq!(artifact.relations.len(), 1);
        assert_eq!(artifact.relations[0].kind, "calls");
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn rejects_unbounded_dimensions_nodes_and_missing_metadata() {
        let _guard = render_test_guard();
        let mut graph = PackedGraph::default();
        let node = graph.add_node("fragment", "one");
        let selection = Selection {
            nodes: vec![node],
            anchors: vec![node],
            distances: [(node, 0)].into(),
            truncated: false,
        };
        let metadata = stable_metadata(&[(node, "fragment:one", "One")]);
        assert!(
            render_packed_graph(
                &graph,
                &selection,
                "one",
                &metadata,
                RenderOptions {
                    width: MIN_RENDER_WIDTH - 1,
                    height: MIN_RENDER_HEIGHT,
                },
            )
            .is_err()
        );
        assert!(
            render_packed_graph(
                &graph,
                &selection,
                "one",
                &BTreeMap::new(),
                RenderOptions::default(),
            )
            .is_err()
        );

        let mut oversized_metadata = metadata.clone();
        oversized_metadata.get_mut(&node).unwrap().title = "x".repeat(MAX_TITLE_BYTES + 1);
        assert!(
            render_packed_graph(
                &graph,
                &selection,
                "one",
                &oversized_metadata,
                RenderOptions::default(),
            )
            .is_err()
        );

        let mut graph = PackedGraph::default();
        let nodes = (0..=MAX_RENDER_NODES)
            .map(|index| graph.add_node("fragment", &format!("fragment-{index}")))
            .collect::<Vec<_>>();
        let selection = Selection {
            nodes,
            anchors: Vec::new(),
            distances: BTreeMap::new(),
            truncated: false,
        };
        assert!(
            render_packed_graph(
                &graph,
                &selection,
                "too many",
                &BTreeMap::new(),
                RenderOptions::default(),
            )
            .is_err()
        );

        let (small_graph, small_selection, small_metadata) = bounded_fixture(5, 0);
        assert!(
            render_packed_graph(
                &small_graph,
                &small_selection,
                "too crowded",
                &small_metadata,
                RenderOptions {
                    width: MIN_RENDER_WIDTH,
                    height: MIN_RENDER_HEIGHT,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn cycles_share_a_component() {
        let mut graph = PackedGraph::default();
        let a = graph.add_node("function", "a");
        let b = graph.add_node("function", "b");
        graph.add_edge(a, b, "calls").unwrap();
        graph.add_edge(b, a, "calls").unwrap();
        let selection = Selection {
            nodes: vec![a, b],
            anchors: vec![a],
            distances: [(a, 0), (b, 1)].into(),
            truncated: false,
        };
        let metadata = stable_metadata(&[(a, "function:a", "A"), (b, "function:b", "B")]);
        let visible = selection.nodes.iter().copied().collect();
        let (edges, _) = selected_edges(&graph, &visible, &metadata).unwrap();
        let components = cycle_components(&selection, &edges, &metadata);
        assert_eq!(components[&a], components[&b]);
    }

    #[test]
    fn relation_and_adjacency_caps_fail_closed_or_report_truncation() {
        let _guard = render_test_guard();
        let (graph, mut selection, metadata) =
            bounded_fixture(MAX_RELATIONS + 2, MAX_RELATIONS + 1);
        selection.anchors = selection.nodes.clone();
        let rendered = render_packed_graph(
            &graph,
            &selection,
            "relation cap",
            &metadata,
            RenderOptions::default(),
        )
        .unwrap();
        assert_eq!(rendered.relations.len(), MAX_RELATIONS);
        assert!(rendered.truncated);

        let mut graph = PackedGraph::default();
        let selected = graph.add_node("fragment", "selected");
        for index in 0..=MAX_ADJACENCY_SCANS {
            let target = graph.add_node("fragment", &format!("unselected-{index}"));
            graph.add_edge(selected, target, "related").unwrap();
        }
        let selection = Selection {
            nodes: vec![selected],
            anchors: vec![selected],
            distances: [(selected, 0)].into(),
            truncated: false,
        };
        let metadata = stable_metadata(&[(selected, "fragment:selected", "Selected")]);
        assert!(
            render_packed_graph(
                &graph,
                &selection,
                "adjacency cap",
                &metadata,
                RenderOptions::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn common_punctuation_and_unknown_text_have_visible_distinct_glyphs() {
        assert_ne!(glyph('·'), glyph('?'));
        assert_ne!(glyph('@'), glyph('?'));
        assert_ne!(glyph('é'), glyph('?'));
        assert_ne!(glyph('é'), [0; 5]);
    }

    #[test]
    fn renderer_identity_is_stable_and_does_not_depend_on_git_state() {
        assert_eq!(RENDERER_PACKAGE_NAME, "spectra-context-render");
        assert_eq!(RENDERER_CRATE_VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(RENDERER_ALGORITHM_REVISION, "scene-v2-direct-png-v10");
        #[cfg(feature = "svg-raster-compat")]
        assert_eq!(
            SVG_RASTER_COMPAT_ALGORITHM_REVISION,
            "canonical-svg-resvg-0.47-v11"
        );
    }

    #[cfg(feature = "svg-raster-compat")]
    #[test]
    fn svg_raster_compat_is_deterministic_and_preserves_scene_pixels() {
        let _guard = render_test_guard();
        let mut graph = PackedGraph::default();
        let nodes = [
            graph.add_node("obligation", "alpha-gate-42"),
            graph.add_node("signal", "violet-signal"),
            graph.add_node("ledger", "immutable-ledger"),
            graph.add_node("validation", "vision-canary"),
            graph.add_node("backend", "direct-png"),
            graph.add_node("backend", "svg-compat"),
        ];
        for endpoints in nodes.windows(2) {
            graph
                .add_edge(endpoints[0], endpoints[1], "verified_by")
                .unwrap();
        }
        let selection = Selection {
            nodes: nodes.to_vec(),
            anchors: vec![nodes[0], nodes[2], nodes[3]],
            distances: nodes.iter().map(|node| (*node, 0)).collect(),
            truncated: false,
        };
        let titles = [
            "ALPHA GATE 42",
            "VIOLET SIGNAL",
            "IMMUTABLE LEDGER",
            "VISION CANARY",
            "DIRECT PNG PATH",
            "SVG COMPAT PATH",
        ];
        let metadata = nodes
            .iter()
            .zip(titles)
            .enumerate()
            .map(|(index, (node, title))| {
                (
                    *node,
                    StableNodeMetadata {
                        stable_ref: format!("vision-proof:{index}"),
                        object_hash: Some(format!("sha256:{index:064x}")),
                        title: title.into(),
                        subtitle: "SPECTRA VISUAL MAP".into(),
                    },
                )
            })
            .collect();
        let query = "VERIFY VISUAL FACTS";
        let rendered = render_packed_graph(
            &graph,
            &selection,
            query,
            &metadata,
            RenderOptions::default(),
        )
        .unwrap();

        let first = rasterize_rendered_svg_compat(&rendered).unwrap();
        let second = rasterize_rendered_svg_compat(&rendered).unwrap();
        let selected_compat = Renderer::new()
            .render_with_backend(
                &graph,
                &selection,
                query,
                &metadata,
                RenderOptions::default(),
                RasterBackend::SvgCompat,
            )
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(selected_compat.png_bytes, first);
        assert_eq!(selected_compat.svg_bytes, rendered.svg_bytes);
        assert_eq!(selected_compat.anchors, rendered.anchors);
        assert_eq!(selected_compat.relations, rendered.relations);
        assert_eq!(&first[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(first[16..20].try_into().unwrap()), 1536);
        assert_eq!(u32::from_be_bytes(first[20..24].try_into().unwrap()), 1024);
        assert_eq!(
            u32::from_be_bytes(rendered.png_bytes[16..20].try_into().unwrap()),
            1536
        );
        assert_eq!(
            u32::from_be_bytes(rendered.png_bytes[20..24].try_into().unwrap()),
            1024
        );

        let (_, _, direct_pixels) = decode_png(&rendered.png_bytes);
        let (compat_width, compat_height, compat_pixels) = decode_png(&first);
        assert_eq!((compat_width, compat_height), (1536, 1024));
        assert_eq!(
            rgba_at(&compat_pixels, compat_width, 0, 0),
            BACKGROUND.rgba,
            "SVG compatibility must preserve the canonical scene background"
        );

        let header = format!("SPECTRA - {query}");
        for character_index in [0, 12, header.len() - 1] {
            let character = header.as_bytes()[character_index] as char;
            let (x, y) = first_filled_glyph_pixel(character, character_index as u32);
            assert_eq!(
                rgba_at(&direct_pixels, compat_width, x, y),
                TEXT.rgba,
                "direct fixture must select a filled header glyph cell"
            );
            assert_eq!(
                rgba_at(&compat_pixels, compat_width, x, y),
                TEXT.rgba,
                "SVG compatibility lost header glyph {character_index} at ({x}, {y})"
            );
        }

        let direct_header_text =
            count_color_in_rows(&direct_pixels, compat_width, 20..35, TEXT.rgba);
        let compat_header_text =
            count_color_in_rows(&compat_pixels, compat_width, 20..35, TEXT.rgba);
        assert!(
            compat_header_text * 100 >= direct_header_text * 95,
            "SVG compatibility retained only {compat_header_text}/{direct_header_text} exact header-text pixels"
        );

        let direct_card_text =
            count_color_in_rows(&direct_pixels, compat_width, 73..107, TEXT.rgba);
        let compat_card_text =
            count_color_in_rows(&compat_pixels, compat_width, 73..107, TEXT.rgba);
        assert!(
            compat_card_text * 100 >= direct_card_text * 95,
            "SVG compatibility retained only {compat_card_text}/{direct_card_text} exact card-text pixels"
        );
    }

    #[cfg(feature = "svg-raster-compat")]
    #[test]
    fn representative_svg_compat_retains_full_map_text_pixels() {
        let _guard = render_test_guard();
        let (graph, selection, metadata) = bounded_fixture(48, 192);
        let rendered = render_packed_graph(
            &graph,
            &selection,
            "representative full-map text",
            &metadata,
            RenderOptions::default(),
        )
        .unwrap();
        let compat = rasterize_rendered_svg_compat(&rendered).unwrap();
        let (width, height, direct_pixels) = decode_png(&rendered.png_bytes);
        let (compat_width, compat_height, compat_pixels) = decode_png(&compat);
        assert_eq!((compat_width, compat_height), (width, height));
        assert!(
            rendered.png_bytes.len() <= 256 * 1024,
            "direct representative PNG grew to {} bytes",
            rendered.png_bytes.len()
        );
        assert!(
            compat.len() <= 384 * 1024,
            "SVG compatibility representative PNG grew to {} bytes",
            compat.len()
        );

        for color in [TEXT.rgba, SUBTEXT.rgba] {
            let direct_count = count_color_in_rows(&direct_pixels, width, 0..height, color);
            let compat_count = count_color_in_rows(&compat_pixels, width, 0..height, color);
            assert!(
                compat_count * 100 >= direct_count * 95,
                "SVG compatibility retained only {compat_count}/{direct_count} full-map text pixels for {color:?}"
            );
        }
    }

    #[cfg(feature = "svg-raster-compat")]
    #[test]
    fn direct_raster_p95_is_bounded_and_faster_than_svg_compat() {
        let _guard = render_test_guard();
        let samples = if cfg!(debug_assertions) { 1 } else { 20 };

        let (graph, selection, metadata) = bounded_fixture(MAX_RENDER_NODES, MAX_RENDER_EDGES);
        let options = RenderOptions::default();
        let scene = build_scene(
            &graph,
            &selection,
            "paired raster benchmark",
            &metadata,
            options,
        )
        .unwrap();
        let rendered = RenderedMap {
            png_bytes: render_scene_png(&scene).unwrap(),
            svg_bytes: render_scene_svg(&scene).into_bytes(),
            anchors: scene.anchors.clone(),
            relations: scene.relations.clone(),
            truncated: scene.truncated,
            node_count: scene.nodes.len(),
            edge_count: scene.edges.len(),
        };

        render_scene_png(&scene).unwrap();
        rasterize_rendered_svg_compat(&rendered).unwrap();

        let mut direct_samples = Vec::with_capacity(samples);
        let mut compat_samples = Vec::with_capacity(samples);
        for _ in 0..samples {
            let started = Instant::now();
            render_scene_png(&scene).unwrap();
            direct_samples.push(started.elapsed());
        }
        for _ in 0..samples {
            let started = Instant::now();
            rasterize_rendered_svg_compat(&rendered).unwrap();
            compat_samples.push(started.elapsed());
        }
        direct_samples.sort_unstable();
        compat_samples.sort_unstable();
        let p95_index = (samples * 95).div_ceil(100) - 1;
        let direct_p95 = direct_samples[p95_index];
        let compat_p95 = compat_samples[p95_index];
        eprintln!(
            "paired raster p95: direct={direct_p95:?}, svg-compat={compat_p95:?}, samples={samples}"
        );

        let direct_ceiling = if cfg!(debug_assertions) {
            Duration::from_secs(3)
        } else {
            Duration::from_millis(60)
        };
        assert!(
            direct_p95 < direct_ceiling,
            "direct raster p95 {direct_p95:?} exceeded {direct_ceiling:?}"
        );
        assert!(
            direct_p95.mul_f64(4.0) <= compat_p95.mul_f64(3.0),
            "direct raster p95 {direct_p95:?} did not preserve the required 25% improvement over SVG compatibility p95 {compat_p95:?}"
        );
    }

    #[cfg(feature = "svg-raster-compat")]
    #[test]
    fn svg_raster_compat_rejects_noncanonical_or_unbounded_input() {
        let _guard = render_test_guard();
        let (graph, selection, metadata) = bounded_fixture(2, 1);
        let mut rendered = render_packed_graph(
            &graph,
            &selection,
            "compatibility bounds",
            &metadata,
            RenderOptions {
                width: 640,
                height: 480,
            },
        )
        .unwrap();

        rendered.svg_bytes = b"<svg/>".to_vec();
        assert!(rasterize_rendered_svg_compat(&rendered).is_err());

        rendered = render_packed_graph(
            &graph,
            &selection,
            "compatibility bounds",
            &metadata,
            RenderOptions {
                width: 640,
                height: 480,
            },
        )
        .unwrap();
        let oversized = String::from_utf8(rendered.svg_bytes).unwrap().replacen(
            "width=\"640\"",
            "width=\"8192\"",
            1,
        );
        rendered.svg_bytes = oversized.into_bytes();
        assert!(rasterize_rendered_svg_compat(&rendered).is_err());
    }

    #[test]
    fn representative_48_node_render_stays_under_warm_budget() {
        let _guard = render_test_guard();
        let (graph, selection, metadata) = bounded_fixture(48, 192);
        let renderer = Renderer::new();
        let options = RenderOptions::default();
        renderer
            .render(
                &graph,
                &selection,
                "representative skills",
                &metadata,
                options,
            )
            .unwrap();
        let started = Instant::now();
        for _ in 0..3 {
            renderer
                .render(
                    &graph,
                    &selection,
                    "representative skills",
                    &metadata,
                    options,
                )
                .unwrap();
        }
        let elapsed = started.elapsed();
        let ceiling = if cfg!(debug_assertions) {
            Duration::from_secs(8)
        } else {
            Duration::from_millis(180)
        };
        eprintln!("three representative warm renders took {elapsed:?}");
        assert!(
            elapsed < ceiling,
            "three representative warm renders took {elapsed:?}, ceiling {ceiling:?}"
        );
    }

    #[test]
    fn hard_ceiling_96_nodes_and_384_edges_is_bounded() {
        let _guard = render_test_guard();
        let (graph, selection, metadata) = bounded_fixture(MAX_RENDER_NODES, MAX_RENDER_EDGES);
        let renderer = Renderer::new();
        let options = RenderOptions::default();
        renderer
            .render(&graph, &selection, "hard ceiling", &metadata, options)
            .unwrap();
        let started = Instant::now();
        let rendered = renderer
            .render(&graph, &selection, "hard ceiling", &metadata, options)
            .unwrap();
        let elapsed = started.elapsed();
        let ceiling = if cfg!(debug_assertions) {
            Duration::from_secs(3)
        } else {
            Duration::from_millis(60)
        };
        assert_eq!(rendered.node_count, MAX_RENDER_NODES);
        assert_eq!(rendered.edge_count, MAX_RENDER_EDGES);
        assert!(!rendered.truncated);
        #[cfg(feature = "svg-raster-compat")]
        {
            let compatibility_png = rasterize_rendered_svg_compat(&rendered).unwrap();
            assert_eq!(
                u32::from_be_bytes(compatibility_png[16..20].try_into().unwrap()),
                options.width
            );
            assert_eq!(
                u32::from_be_bytes(compatibility_png[20..24].try_into().unwrap()),
                options.height
            );
        }
        eprintln!("hard-ceiling warm render took {elapsed:?}");
        assert!(
            elapsed < ceiling,
            "hard-ceiling warm render took {elapsed:?}, ceiling {ceiling:?}"
        );
    }

    #[test]
    fn warm_in_memory_path_is_bounded_and_ignores_unselected_graph_size() {
        let _guard = render_test_guard();
        let mut graph = PackedGraph::default();
        let selected = graph.add_node("fragment", "selected");
        for index in 0..20_000 {
            graph.add_node("unused", &format!("unused-{index}"));
        }
        let selection = Selection {
            nodes: vec![selected],
            anchors: vec![selected],
            distances: [(selected, 0)].into(),
            truncated: false,
        };
        let metadata = stable_metadata(&[(selected, "fragment:selected", "Selected fragment")]);
        let renderer = Renderer::new();
        let options = RenderOptions::default();
        renderer
            .render(&graph, &selection, "selected", &metadata, options)
            .unwrap();
        let started = Instant::now();
        for _ in 0..3 {
            renderer
                .render(&graph, &selection, "selected", &metadata, options)
                .unwrap();
        }
        let elapsed = started.elapsed();
        let ceiling = if cfg!(debug_assertions) {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(180)
        };
        assert!(
            elapsed < ceiling,
            "three warm renders took {elapsed:?}, ceiling {ceiling:?}"
        );
    }
}
