use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, identifier_names, named_child_by_kind,
    terminal_identifier,
};

pub(crate) struct CSharpAdapter;
pub(crate) static CSHARP: CSharpAdapter = CSharpAdapter;

impl LanguageAdapter for CSharpAdapter {
    fn id(&self) -> &'static str {
        "csharp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn classify(&self, node: SyntaxNode<'_>, _scopes: &[Scope]) -> Option<&'static str> {
        match node.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => Some("module"),
            "class_declaration" | "record_declaration" => Some("class"),
            "interface_declaration" => Some("interface"),
            "struct_declaration" | "record_struct_declaration" => Some("struct"),
            "enum_declaration" => Some("enum"),
            "method_declaration" | "constructor_declaration" => Some("method"),
            "using_directive" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "invocation_expression")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "using_directive" {
            return terminal_identifier(node, source)
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        named_child_by_kind(node, "base_list")
            .into_iter()
            .flat_map(|bases| identifier_names(bases, source))
            .map(|target| Relation {
                kind: "inherits",
                target,
            })
            .collect()
    }
}
