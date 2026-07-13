use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, identifier_names, inside_type, leading_identifier,
    named_child_by_kind, terminal_identifier,
};

pub(crate) struct KotlinAdapter;
pub(crate) static KOTLIN: KotlinAdapter = KotlinAdapter;

impl LanguageAdapter for KotlinAdapter {
    fn id(&self) -> &'static str {
        "kotlin"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_declaration" => {
                let prefix = node
                    .child_by_field_name("name")
                    .and_then(|name| source.get(node.start_byte()..name.start_byte()))
                    .and_then(|prefix| std::str::from_utf8(prefix).ok())
                    .unwrap_or("");
                let has_keyword = |expected| {
                    prefix
                        .split(|character: char| !character.is_alphanumeric())
                        .any(|keyword| keyword == expected)
                };
                if has_keyword("interface") {
                    Some("interface")
                } else if has_keyword("enum") {
                    Some("enum")
                } else {
                    Some("class")
                }
            }
            "function_declaration" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "type_alias" => Some("type_alias"),
            "import_header" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| leading_identifier(node, source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "import_header" {
            return terminal_identifier(node, source)
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        named_child_by_kind(node, "delegation_specifiers")
            .into_iter()
            .flat_map(|bases| identifier_names(bases, source))
            .map(|target| Relation {
                kind: "inherits",
                target,
            })
            .collect()
    }
}
