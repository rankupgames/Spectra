use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{LanguageAdapter, Relation, Scope, field_text, identifier_names, terminal_identifier};

pub(crate) struct JavaAdapter;
pub(crate) static JAVA: JavaAdapter = JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    fn id(&self) -> &'static str {
        "java"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn classify(&self, node: SyntaxNode<'_>, _scopes: &[Scope]) -> Option<&'static str> {
        match node.kind() {
            "package_declaration" => Some("module"),
            "class_declaration" | "record_declaration" => Some("class"),
            "interface_declaration" | "annotation_type_declaration" => Some("interface"),
            "enum_declaration" => Some("enum"),
            "method_declaration" | "constructor_declaration" => Some("method"),
            "import_declaration" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "method_invocation")
            .then(|| field_text(node, "name", source).map(str::to_owned))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        let mut relations = Vec::new();
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
        if let Some(superclass) = node.child_by_field_name("superclass") {
            relations.extend(
                identifier_names(superclass, source)
                    .into_iter()
                    .map(|target| Relation {
                        kind: "extends",
                        target,
                    }),
            );
        }
        if let Some(interfaces) = node.child_by_field_name("interfaces") {
            relations.extend(
                identifier_names(interfaces, source)
                    .into_iter()
                    .map(|target| Relation {
                        kind: "implements",
                        target,
                    }),
            );
        }
        relations
    }
}
