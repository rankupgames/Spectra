use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, field_text, identifier_names, inside_type,
    javascript::variable_function_name, named_child_by_kind, text, truncate,
};

pub(crate) struct TypeScriptAdapter;
pub(crate) static TYPESCRIPT: TypeScriptAdapter = TypeScriptAdapter;

impl LanguageAdapter for TypeScriptAdapter {
    fn id(&self) -> &'static str {
        "typescript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }

    fn language(&self, path: &Path) -> Option<Language> {
        if path.extension().is_some_and(|extension| extension == "tsx") {
            Some(tree_sitter_typescript::LANGUAGE_TSX.into())
        } else {
            Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        }
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_declaration" | "abstract_class_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "type_alias_declaration" => Some("type_alias"),
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
        if !matches!(
            node.kind(),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
        ) {
            return Vec::new();
        }
        let Some(heritage) = named_child_by_kind(node, "class_heritage") else {
            return Vec::new();
        };
        let mut cursor = heritage.walk();
        heritage
            .named_children(&mut cursor)
            .flat_map(|clause| {
                let kind = if clause.kind() == "implements_clause" {
                    "implements"
                } else {
                    "extends"
                };
                identifier_names(clause, source)
                    .into_iter()
                    .map(move |target| Relation { kind, target })
            })
            .collect()
    }
}
