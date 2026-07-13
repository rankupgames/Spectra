use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, identifier_names, named_child_by_kind,
    terminal_identifier,
};

pub(crate) struct ScalaAdapter;
pub(crate) static SCALA: ScalaAdapter = ScalaAdapter;

impl LanguageAdapter for ScalaAdapter {
    fn id(&self) -> &'static str {
        "scala"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["scala", "sc"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_scala::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_definition" => Some("class"),
            "trait_definition" => Some("trait"),
            "object_definition" => Some("module"),
            "enum_definition" => Some("enum"),
            "function_definition" | "function_declaration" => {
                let member = scopes
                    .iter()
                    .rev()
                    .take_while(|scope| !matches!(scope.kind, "function" | "method"))
                    .any(|scope| matches!(scope.kind, "class" | "trait" | "interface" | "module"));
                Some(if member { "method" } else { "function" })
            }
            "type_definition" => Some("type_alias"),
            "import_declaration" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "import_declaration" {
            return terminal_identifier(node, source)
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        named_child_by_kind(node, "extends_clause")
            .into_iter()
            .flat_map(|bases| identifier_names(bases, source))
            .map(|target| Relation {
                kind: "inherits",
                target,
            })
            .collect()
    }
}
