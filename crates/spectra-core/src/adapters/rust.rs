use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode, Parser};

use super::{
    EmbeddedRegion, FileSymbol, LanguageAdapter, Relation, Scope, call_target, field_text,
    frameworks, inside_type, text, truncate,
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

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_rust::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
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

    fn file_symbols(&self, _path: &Path, source: &str) -> Vec<FileSymbol> {
        frameworks::rust_routes(source)
    }

    fn embedded_regions(&self, source: &str) -> Vec<EmbeddedRegion> {
        let mut parser = Parser::new();
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        if parser.set_language(&language).is_err() {
            return Vec::new();
        }
        let Some(tree) = parser.parse(source, None) else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        root.named_children(&mut cursor)
            .filter(|node| node.kind() == "macro_invocation")
            .filter_map(|node| item_macro_body(node, source, &language))
            .collect()
    }
}

fn item_macro_body(
    invocation: SyntaxNode<'_>,
    source: &str,
    language: &Language,
) -> Option<EmbeddedRegion> {
    let mut cursor = invocation.walk();
    let token_tree = invocation
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let range = token_tree.byte_range();
    let body_start = range.start.checked_add(1)?;
    let body_end = range.end.checked_sub(1)?;
    let fragment = source.get(body_start..body_end)?;
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    let tree = parser.parse(fragment, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut cursor = root.walk();
    let contains_item = root.named_children(&mut cursor).any(|child| {
        matches!(
            child.kind(),
            "const_item"
                | "enum_item"
                | "function_item"
                | "impl_item"
                | "macro_definition"
                | "mod_item"
                | "static_item"
                | "struct_item"
                | "trait_item"
                | "type_item"
                | "use_declaration"
        )
    });
    contains_item.then_some(EmbeddedRegion {
        language: "rust",
        start_byte: body_start,
        end_byte: body_end,
    })
}
