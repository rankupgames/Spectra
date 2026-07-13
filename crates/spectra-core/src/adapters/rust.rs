use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, field_text, inside_type, text, truncate,
};

pub(crate) struct RustAdapter;
pub(crate) static RUST: RustAdapter = RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn classify(&self, node: SyntaxNode<'_>, scopes: &[Scope]) -> Option<&'static str> {
        match node.kind() {
            "mod_item" => Some("module"),
            "struct_item" => Some("struct"),
            "enum_item" => Some("enum"),
            "trait_item" => Some("trait"),
            "impl_item" => Some("impl"),
            "function_item" | "function_signature_item" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "type_item" => Some("type_alias"),
            "const_item" => Some("constant"),
            "static_item" => Some("static"),
            "macro_definition" => Some("macro"),
            "use_declaration" => Some("import"),
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
        if mapped_kind == "impl" {
            return field_text(node, "type", source).map(|value| truncate(value, 56));
        }
        field_text(node, "name", source).map(str::to_owned)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "use_declaration" {
            return node
                .child_by_field_name("argument")
                .and_then(|argument| super::terminal_identifier(argument, source))
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        if node.kind() != "impl_item" {
            return Vec::new();
        }
        field_text(node, "trait", source)
            .and_then(|name| name.rsplit("::").next())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|target| {
                vec![Relation {
                    kind: "implements",
                    target: target.to_owned(),
                }]
            })
            .unwrap_or_default()
    }
}
