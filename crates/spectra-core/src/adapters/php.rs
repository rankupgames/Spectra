use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, field_text, identifier_names,
    named_child_by_kind, string_literal_value, terminal_identifier, text, truncate,
};

pub(crate) struct PhpAdapter;
pub(crate) static PHP: PhpAdapter = PhpAdapter;

impl LanguageAdapter for PhpAdapter {
    fn id(&self) -> &'static str {
        "php"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["php", "module", "install", "theme", "inc"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_php::LANGUAGE_PHP.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "namespace_definition" => Some("module"),
            "class_declaration" => Some("class"),
            "trait_declaration" => Some("trait"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "function_definition" => Some("function"),
            "method_declaration" => Some("method"),
            "namespace_use_declaration"
            | "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => Some("import"),
            _ => None,
        }
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        if mapped_kind == "import" {
            return Some(truncate(
                text(node, source).trim().trim_end_matches(';'),
                72,
            ));
        }
        field_text(node, "name", source).map(str::to_owned)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        match node.kind() {
            "function_call_expression" => call_target(node, "function", source),
            "member_call_expression" | "scoped_call_expression" => {
                call_target(node, "name", source)
            }
            _ => None,
        }
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "namespace_use_declaration" {
            return terminal_identifier(node, source)
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        if matches!(
            node.kind(),
            "include_expression"
                | "include_once_expression"
                | "require_expression"
                | "require_once_expression"
        ) {
            return string_literal_value(node, source)
                .and_then(|path| path.rsplit('/').next().map(str::to_owned))
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        let mut relations = Vec::new();
        for clause_kind in ["base_clause", "class_interface_clause"] {
            if let Some(clause) = named_child_by_kind(node, clause_kind) {
                relations.extend(identifier_names(clause, source).into_iter().map(|target| {
                    Relation {
                        kind: "inherits",
                        target,
                    }
                }));
            }
        }
        relations
    }
}
