use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, field_text, identifier_names, inside_type,
    named_child_by_kind, terminal_identifier, text, truncate,
};

pub(crate) struct JavaScriptAdapter;
pub(crate) static JAVASCRIPT: JavaScriptAdapter = JavaScriptAdapter;

impl LanguageAdapter for JavaScriptAdapter {
    fn id(&self) -> &'static str {
        "javascript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_declaration" => Some("class"),
            "method_definition" => Some("method"),
            "function_declaration" | "function_expression" | "arrow_function" => {
                Some(if inside_type(scopes) {
                    "method"
                } else {
                    "function"
                })
            }
            "import_statement" => Some("import"),
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
        field_text(node, "name", source)
            .map(str::to_owned)
            .or_else(|| variable_function_name(node, source))
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "import_statement" {
            return node
                .child_by_field_name("import_clause")
                .or_else(|| named_child_by_kind(node, "import_clause"))
                .into_iter()
                .flat_map(|clause| identifier_names(clause, source))
                .map(|target| Relation {
                    kind: "imports",
                    target,
                })
                .collect();
        }
        if node.kind() != "class_declaration" {
            return Vec::new();
        }
        named_child_by_kind(node, "class_heritage")
            .into_iter()
            .filter_map(|heritage| terminal_identifier(heritage, source))
            .map(|target| Relation {
                kind: "extends",
                target,
            })
            .collect()
    }
}

pub(crate) fn variable_function_name(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    (parent.kind() == "variable_declarator")
        .then(|| field_text(parent, "name", source).map(str::to_owned))
        .flatten()
}
