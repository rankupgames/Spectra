use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, descendant_field_text, identifier_names,
    inside_type, named_child_by_kind, string_literal_value, text, truncate,
};

pub(crate) struct DartAdapter;
pub(crate) static DART: DartAdapter = DartAdapter;

impl LanguageAdapter for DartAdapter {
    fn id(&self) -> &'static str {
        "dart"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["dart"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_dart::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_declaration" | "mixin_declaration" => Some("class"),
            "extension_declaration" => Some("impl"),
            "enum_declaration" => Some("enum"),
            "function_declaration" | "local_function_declaration" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "method_declaration" => Some("method"),
            "type_alias" => Some("type_alias"),
            "import_or_export" => Some("import"),
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
        descendant_field_text(node, "name", source).map(str::to_owned)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "import_or_export" {
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
        for field in ["superclass", "interfaces"] {
            if let Some(types) = node.child_by_field_name(field) {
                relations.extend(identifier_names(types, source).into_iter().map(|target| {
                    Relation {
                        kind: "inherits",
                        target,
                    }
                }));
            }
        }
        if let Some(mixins) = named_child_by_kind(node, "mixins") {
            relations.extend(
                identifier_names(mixins, source)
                    .into_iter()
                    .map(|target| Relation {
                        kind: "inherits",
                        target,
                    }),
            );
        }
        relations
    }
}
