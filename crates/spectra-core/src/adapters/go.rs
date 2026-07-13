use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{LanguageAdapter, Relation, Scope, call_target, field_text, text};

pub(crate) struct GoAdapter;
pub(crate) static GO: GoAdapter = GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn id(&self) -> &'static str {
        "go"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "package_clause" => Some("module"),
            "function_declaration" => Some("function"),
            "method_declaration" => Some("method"),
            "type_spec" => node.child_by_field_name("type").map(|ty| match ty.kind() {
                "struct_type" => "struct",
                "interface_type" => "interface",
                _ => "type_alias",
            }),
            "import_spec" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        if mapped_kind == "import" {
            return Some(text(node, source).trim().to_owned());
        }
        field_text(node, "name", source).map(str::to_owned)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() != "import_spec" {
            return Vec::new();
        }
        let target = node
            .child_by_field_name("path")
            .map(|path| text(path, source).trim_matches('"'))
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty());
        target
            .map(|target| {
                vec![Relation {
                    kind: "imports",
                    target: target.to_owned(),
                }]
            })
            .unwrap_or_default()
    }
}
