use std::{fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use spectra_core::{MapArtifact, map_project};

#[derive(Clone, Debug, Default)]
struct SpectraServer;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct MapRequest {
    /// Architecture question, symbol names, or file/path terms to focus on.
    query: String,
    /// Project directory. Defaults to the MCP server's working directory.
    project_path: Option<String>,
    /// Maximum visible nodes. Defaults to 48 and is capped at 96.
    #[schemars(range(min = 1, max = 96))]
    max_nodes: Option<u16>,
}

#[tool_router]
impl SpectraServer {
    #[tool(
        name = "spectra_map",
        description = "Render a compact PNG code-topology map for a Rust architecture question. Returns an image plus exact file/line anchors, never source bodies."
    )]
    async fn spectra_map(&self, Parameters(request): Parameters<MapRequest>) -> CallToolResult {
        match build_map_result(request) {
            Ok(result) => result,
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "spectra_map failed: {error}"
            ))]),
        }
    }
}

#[tool_handler(
    name = "spectra",
    version = "0.1.0",
    instructions = "Use spectra_map for Rust architecture and navigation questions. Inspect the PNG, then read only the exact anchor selected for editing."
)]
impl ServerHandler for SpectraServer {}

fn build_map_result(request: MapRequest) -> Result<CallToolResult, Box<dyn std::error::Error>> {
    let project = request
        .project_path
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let output = project.join(".spectra/artifacts");
    let artifact = map_project(
        &project,
        &request.query,
        usize::from(request.max_nodes.unwrap_or(48).clamp(1, 96)),
        &output,
    )?;
    let png = fs::read(&artifact.png_path)?;
    let metadata = compact_metadata(&artifact);
    Ok(CallToolResult::success(vec![
        ContentBlock::image(STANDARD.encode(png), "image/png"),
        ContentBlock::text(metadata),
    ]))
}

fn compact_metadata(artifact: &MapArtifact) -> String {
    let mut lines = Vec::with_capacity(artifact.anchors.len() + 1);
    for (id, anchor) in &artifact.anchors {
        lines.push(format!(
            "{id}={}:{}-{}",
            anchor.path, anchor.start_line, anchor.end_line
        ));
    }
    lines.push(format!(
        "nodes={} truncated={} index=v{}",
        artifact.node_count, artifact.truncated, artifact.index_version
    ));
    lines.join("\n")
}

pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let service = SpectraServer.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServerHandler;
    use spectra_core::{MapArtifact, SourceAnchor};

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
                            path: format!("src/module_{number}/implementation.rs"),
                            start_line: number * 10,
                            end_line: number * 10 + 8,
                        },
                    )
                })
                .collect(),
            truncated: false,
            node_count: 48,
            index_version: 1,
        };
        let metadata = compact_metadata(&artifact);
        assert!(metadata.chars().count().div_ceil(4) < 200);
        assert!(!metadata.contains("fn "));
    }

    #[test]
    fn server_identifies_itself_and_lists_exactly_one_tool() {
        let server = SpectraServer;
        assert_eq!(server.get_info().server_info.name, "spectra");
        let tools = SpectraServer::tool_router().list_all();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "spectra_map");
    }
}
