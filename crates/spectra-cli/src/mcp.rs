use std::{collections::BTreeSet, fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    handler::server::tool::ToolCallContext,
    handler::server::wrapper::Parameters,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use spectra_core::{MapArtifact, map_project};

use crate::autosync::{AutoSync, SyncSnapshot};
use crate::mcp_query::{self, Direction, FileFormat, NodeViewOptions};

#[derive(Clone, Default)]
struct SpectraServer {
    autosync: AutoSync,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct MapRequest {
    /// Architecture question, symbol names, or file/path terms to focus on.
    query: String,
    /// Project directory. Defaults to the MCP server's working directory.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
    /// Maximum visible nodes. Defaults to 48 and is capped at 96.
    #[schemars(range(min = 1, max = 96))]
    #[serde(rename = "maxNodes", alias = "max_nodes")]
    max_nodes: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct ExploreRequest {
    /// Symbol names, file names, or a natural-language code-flow question.
    query: String,
    /// Maximum files whose bounded source windows are returned. Defaults to 8.
    #[serde(rename = "maxFiles", alias = "max_files")]
    #[schemars(range(min = 1, max = 20))]
    max_files: Option<u8>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct SearchRequest {
    /// Symbol name or partial name.
    query: String,
    /// Optional node-kind filter.
    kind: Option<SearchKindRequest>,
    /// Maximum results. Defaults to 10 and is capped at 100.
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u8>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SearchKindRequest {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Variable,
    Route,
    Component,
}

impl SearchKindRequest {
    fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Variable => "variable",
            Self::Route => "route",
            Self::Component => "component",
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct RelationshipRequest {
    /// Function, method, class, route, or component name.
    symbol: String,
    /// Optional path or suffix used to disambiguate same-named definitions.
    file: Option<String>,
    /// Maximum related symbols. Defaults to 20 and is capped at 100.
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u8>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct ImpactRequest {
    /// Symbol whose dependent blast radius should be traversed.
    symbol: String,
    /// Optional path or suffix used to disambiguate same-named definitions.
    file: Option<String>,
    /// Dependency depth. Defaults to 2 and is capped at 10.
    #[schemars(range(min = 1, max = 10))]
    depth: Option<u8>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct NodeRequest {
    /// Symbol to inspect. Omit and pass file alone for file-read mode.
    symbol: Option<String>,
    /// File path or basename, alone for file mode or with symbol to disambiguate.
    file: Option<String>,
    /// Include a bounded, line-numbered source body in symbol mode.
    #[serde(rename = "includeCode", alias = "include_code")]
    include_code: Option<bool>,
    /// One-based starting line in file mode.
    offset: Option<usize>,
    /// Maximum lines in file mode, capped at 2000.
    #[schemars(range(min = 1, max = 2000))]
    limit: Option<usize>,
    /// Return only the file symbol map without source.
    #[serde(rename = "symbolsOnly", alias = "symbols_only")]
    symbols_only: Option<bool>,
    /// Definition line used to disambiguate a symbol.
    line: Option<u32>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct StatusRequest {
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum FileFormatRequest {
    #[default]
    Tree,
    Flat,
    Grouped,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct FilesRequest {
    /// Filter to files under this project-relative directory.
    path: Option<String>,
    /// Filter with a glob such as `*.tsx` or `**/*.test.ts`.
    pattern: Option<String>,
    /// Output format: tree, flat, or grouped.
    format: Option<FileFormatRequest>,
    /// Include language and symbol counts. Defaults to true.
    #[serde(rename = "includeMetadata", alias = "include_metadata")]
    include_metadata: Option<bool>,
    /// Maximum directory depth in tree mode.
    #[serde(rename = "maxDepth", alias = "max_depth")]
    #[schemars(range(min = 1, max = 20))]
    max_depth: Option<u8>,
    /// Absolute project path, or any directory inside the project.
    #[serde(rename = "projectPath", alias = "project_path")]
    project_path: Option<String>,
}

#[tool_router]
impl SpectraServer {
    #[tool(
        name = "spectra_map",
        description = "Render a compact PNG code-topology map for a polyglot architecture question. Returns an image plus exact file/line anchors, never source bodies.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn spectra_map(&self, Parameters(request): Parameters<MapRequest>) -> CallToolResult {
        match build_map_result(&self.autosync, request) {
            Ok(result) => result,
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "spectra_map failed: {error}"
            ))]),
        }
    }

    #[tool(
        name = "spectra_explore",
        description = "PRIMARY TEXT TOOL — return bounded, line-numbered source for the files and symbols relevant to a code question, plus relationships among them in one call.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_explore(
        &self,
        Parameters(request): Parameters<ExploreRequest>,
    ) -> CallToolResult {
        if let Some(error) = validate_required(&request.query, "query") {
            return error;
        }
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_explore",
            |view| {
                mcp_query::explore(
                    view,
                    &request.query,
                    usize::from(request.max_files.unwrap_or(8)),
                )
            },
        )
    }

    #[tool(
        name = "spectra_search",
        description = "Quick symbol search by name. Returns locations only; use spectra_explore for source and flow context.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_search(
        &self,
        Parameters(request): Parameters<SearchRequest>,
    ) -> CallToolResult {
        if let Some(error) = validate_required(&request.query, "query") {
            return error;
        }
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_search",
            |view| {
                mcp_query::search(
                    view,
                    &request.query,
                    request.kind.map(SearchKindRequest::as_str),
                    usize::from(request.limit.unwrap_or(10)),
                )
            },
        )
    }

    #[tool(
        name = "spectra_callers",
        description = "List functions, routes, or components that call or dispatch to a symbol.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_callers(
        &self,
        Parameters(request): Parameters<RelationshipRequest>,
    ) -> CallToolResult {
        relationship_result(
            &self.autosync,
            request,
            Direction::Callers,
            "spectra_callers",
        )
    }

    #[tool(
        name = "spectra_callees",
        description = "List functions, routes, or components reached by a symbol.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_callees(
        &self,
        Parameters(request): Parameters<RelationshipRequest>,
    ) -> CallToolResult {
        relationship_result(
            &self.autosync,
            request,
            Direction::Callees,
            "spectra_callees",
        )
    }

    #[tool(
        name = "spectra_impact",
        description = "List symbols affected by changing a symbol, traversed through incoming dependency edges.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_impact(
        &self,
        Parameters(request): Parameters<ImpactRequest>,
    ) -> CallToolResult {
        if let Some(error) = validate_required(&request.symbol, "symbol") {
            return error;
        }
        if let Some(error) = validate_optional_path(request.file.as_deref(), "file") {
            return error;
        }
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_impact",
            |view| {
                mcp_query::impact(
                    view,
                    &request.symbol,
                    request.file.as_deref(),
                    usize::from(request.depth.unwrap_or(2)),
                )
            },
        )
    }

    #[tool(
        name = "spectra_node",
        description = "Inspect one symbol with its caller/callee trail, or pass file alone for a line-numbered file view. Configuration values are withheld.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_node(&self, Parameters(request): Parameters<NodeRequest>) -> CallToolResult {
        if request.symbol.is_none() && request.file.is_none() {
            return CallToolResult::error(vec![ContentBlock::text(
                "spectra_node requires `symbol`, or `file` for file mode",
            )]);
        }
        if let Some(error) = validate_optional_path(request.file.as_deref(), "file") {
            return error;
        }
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_node",
            |view| {
                mcp_query::node_view(
                    view,
                    NodeViewOptions {
                        symbol: request.symbol.as_deref(),
                        file: request.file.as_deref(),
                        line: request.line,
                        include_code: request.include_code.unwrap_or(false),
                        offset: request.offset,
                        limit: request.limit,
                        symbols_only: request.symbols_only.unwrap_or(false),
                    },
                )
            },
        )
    }

    #[tool(
        name = "spectra_status",
        description = "Index and auto-sync health check: files, nodes, edges, languages, and pending watcher state.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_status(
        &self,
        Parameters(request): Parameters<StatusRequest>,
    ) -> CallToolResult {
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_status",
            mcp_query::status,
        )
    }

    #[tool(
        name = "spectra_files",
        description = "Indexed file tree with language and symbol counts, filterable by path and glob.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn spectra_files(&self, Parameters(request): Parameters<FilesRequest>) -> CallToolResult {
        for (value, field) in [
            (request.path.as_deref(), "path"),
            (request.pattern.as_deref(), "pattern"),
        ] {
            if let Some(error) = validate_optional_path(value, field) {
                return error;
            }
        }
        let format = match request.format.unwrap_or_default() {
            FileFormatRequest::Tree => FileFormat::Tree,
            FileFormatRequest::Flat => FileFormat::Flat,
            FileFormatRequest::Grouped => FileFormat::Grouped,
        };
        query_result(
            &self.autosync,
            request.project_path.as_deref(),
            "spectra_files",
            |view| {
                mcp_query::files(
                    view,
                    request.path.as_deref(),
                    request.pattern.as_deref(),
                    format,
                    request.include_metadata.unwrap_or(true),
                    request.max_depth.map(usize::from),
                )
            },
        )
    }
}

#[tool_handler(
    name = "spectra",
    version = "0.2.0",
    instructions = "Use spectra_map for visual architecture questions. Enable spectra_explore for bounded source and flow context; targeted search, node, caller/callee, impact, file-tree, and status tools are available through SPECTRA_MCP_TOOLS."
)]
impl ServerHandler for SpectraServer {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let allowed = configured_tools();
        Ok(ListToolsResult {
            tools: SpectraServer::tool_router()
                .list_all()
                .into_iter()
                .filter(|tool| allowed.contains(tool.name.as_ref()))
                .collect(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if !configured_tools().contains(request.name.as_ref()) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Tool {} is disabled via SPECTRA_MCP_TOOLS",
                request.name
            ))]));
        }
        let tool_context = ToolCallContext::new(self, request, context);
        SpectraServer::tool_router().call(tool_context).await
    }
}

fn validate_required(value: &str, field: &str) -> Option<CallToolResult> {
    if value.trim().is_empty() {
        return Some(CallToolResult::error(vec![ContentBlock::text(format!(
            "{field} must be a non-empty string"
        ))]));
    }
    if value.len() > 10_000 {
        return Some(CallToolResult::error(vec![ContentBlock::text(format!(
            "{field} exceeds 10000 characters"
        ))]));
    }
    None
}

fn validate_optional_path(value: Option<&str>, field: &str) -> Option<CallToolResult> {
    value.filter(|value| value.len() > 4_096).map(|_| {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "{field} exceeds 4096 characters"
        ))])
    })
}

fn relationship_result(
    autosync: &AutoSync,
    request: RelationshipRequest,
    direction: Direction,
    tool: &str,
) -> CallToolResult {
    if let Some(error) = validate_required(&request.symbol, "symbol") {
        return error;
    }
    if let Some(error) = validate_optional_path(request.file.as_deref(), "file") {
        return error;
    }
    query_result(autosync, request.project_path.as_deref(), tool, |view| {
        mcp_query::relationships(
            view,
            &request.symbol,
            request.file.as_deref(),
            direction,
            usize::from(request.limit.unwrap_or(20)),
        )
    })
}

fn query_result(
    autosync: &AutoSync,
    project_path: Option<&str>,
    tool: &str,
    render: impl FnOnce(&mcp_query::ProjectView) -> String,
) -> CallToolResult {
    match mcp_query::open_project(autosync, project_path) {
        Ok(view) => CallToolResult::success(vec![ContentBlock::text(render(&view))]),
        Err(error) => CallToolResult::success(vec![ContentBlock::text(format!(
            "{tool} could not open an indexed project: {error}. Pass projectPath for another repository or run `spectra init`."
        ))]),
    }
}

fn configured_tools() -> BTreeSet<String> {
    allowed_tools(std::env::var("SPECTRA_MCP_TOOLS").ok().as_deref())
}

fn allowed_tools(raw: Option<&str>) -> BTreeSet<String> {
    let all = SpectraServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<BTreeSet<_>>();
    let Some(raw) = raw.filter(|raw| !raw.trim().is_empty()) else {
        return ["spectra_map".to_owned()].into_iter().collect();
    };
    if raw.trim() == "all" {
        return all;
    }
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            if name.starts_with("spectra_") {
                name.to_owned()
            } else {
                format!("spectra_{name}")
            }
        })
        .filter(|name| all.contains(name))
        .collect()
}

fn build_map_result(
    autosync: &AutoSync,
    request: MapRequest,
) -> Result<CallToolResult, Box<dyn std::error::Error>> {
    let project = request
        .project_path
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let sync = autosync.ensure_project(&project);
    let output = project.join(".spectra/artifacts");
    let artifact = map_project(
        &project,
        &request.query,
        usize::from(request.max_nodes.unwrap_or(48).clamp(1, 96)),
        &output,
    )?;
    let png = fs::read(&artifact.png_path)?;
    let metadata = compact_metadata(&artifact, Some(&sync));
    Ok(CallToolResult::success(vec![
        ContentBlock::image(STANDARD.encode(png), "image/png"),
        ContentBlock::text(metadata),
    ]))
}

fn compact_metadata(artifact: &MapArtifact, sync: Option<&SyncSnapshot>) -> String {
    let mut metadata = artifact.compact_metadata();
    if let Some(sync) = sync {
        metadata.push('\n');
        metadata.push_str(&sync.compact());
    }
    metadata
}

pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let server = SpectraServer::default();
    let status = server.autosync.ensure_project(&std::env::current_dir()?);
    eprintln!("spectra: {}", status.compact());
    if !status.active {
        eprintln!(
            "spectra: live watching is degraded; use `spectra autosync install` for Git-based fallback"
        );
    }
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServerHandler;
    use spectra_core::{MapArtifact, MapRelation, SourceAnchor};

    #[test]
    fn metadata_stays_well_below_the_default_budget() {
        let artifact = MapArtifact {
            png_path: PathBuf::new(),
            svg_path: PathBuf::new(),
            anchors: (1..=12)
                .map(|number| {
                    (
                        format!("N{number}"),
                        SourceAnchor {
                            kind: "method".into(),
                            qualified_name: format!("module_{number}::Worker::execute"),
                            path: format!("src/module_{number}/implementation.rs"),
                            start_line: number * 10,
                            end_line: number * 10 + 8,
                        },
                    )
                })
                .collect(),
            relations: vec![MapRelation {
                source: "N1".into(),
                kind: "calls".into(),
                target: "N2".into(),
            }],
            truncated: false,
            node_count: 48,
            index_version: 1,
        };
        let metadata = compact_metadata(&artifact, None);
        assert!(metadata.chars().count().div_ceil(4) < 400);
        assert!(metadata.contains("N1=method module_1::Worker::execute @ src/module_1"));
        assert!(metadata.contains("flow N1 -calls-> N2"));
        assert!(!metadata.contains("fn "));
    }

    #[test]
    fn server_pins_the_codegraph_parity_tool_contract() {
        let server = SpectraServer::default();
        assert_eq!(server.get_info().server_info.name, "spectra");
        assert_eq!(
            server.get_info().server_info.version,
            env!("CARGO_PKG_VERSION")
        );
        let tools = SpectraServer::tool_router().list_all();
        let names = tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            [
                "spectra_map",
                "spectra_explore",
                "spectra_search",
                "spectra_callers",
                "spectra_callees",
                "spectra_impact",
                "spectra_node",
                "spectra_status",
                "spectra_files",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(allowed_tools(None), ["spectra_map".to_owned()].into());
        assert_eq!(
            allowed_tools(Some("explore,node,status")),
            ["spectra_explore", "spectra_node", "spectra_status"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(allowed_tools(Some("all")).len(), 9);
        for tool in tools.iter().filter(|tool| tool.name != "spectra_map") {
            let annotations = tool.annotations.as_ref().expect("read tool annotations");
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.idempotent_hint, Some(true));
            let schema = serde_json::to_value(&tool.input_schema).unwrap();
            assert!(schema["properties"].get("projectPath").is_some());
        }
        let expected_properties = [
            ("spectra_explore", &["query", "maxFiles", "projectPath"][..]),
            (
                "spectra_search",
                &["query", "kind", "limit", "projectPath"][..],
            ),
            (
                "spectra_callers",
                &["symbol", "file", "limit", "projectPath"][..],
            ),
            (
                "spectra_callees",
                &["symbol", "file", "limit", "projectPath"][..],
            ),
            (
                "spectra_impact",
                &["symbol", "file", "depth", "projectPath"][..],
            ),
            (
                "spectra_node",
                &[
                    "symbol",
                    "includeCode",
                    "file",
                    "offset",
                    "limit",
                    "symbolsOnly",
                    "line",
                    "projectPath",
                ][..],
            ),
            ("spectra_status", &["projectPath"][..]),
            (
                "spectra_files",
                &[
                    "path",
                    "pattern",
                    "format",
                    "includeMetadata",
                    "maxDepth",
                    "projectPath",
                ][..],
            ),
        ];
        for (name, properties) in expected_properties {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            let schema = serde_json::to_value(&tool.input_schema).unwrap();
            let actual = schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                actual,
                properties.iter().copied().collect::<BTreeSet<_>>(),
                "{name} schema drifted"
            );
        }
    }
}
