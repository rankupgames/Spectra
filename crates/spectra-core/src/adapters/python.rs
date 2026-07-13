use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{LanguageAdapter, Relation, Scope, call_target, identifier_names, inside_type};

pub(crate) struct PythonAdapter;
pub(crate) static PYTHON: PythonAdapter = PythonAdapter;

impl LanguageAdapter for PythonAdapter {
    fn id(&self) -> &'static str {
        "python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_definition" => Some("class"),
            "function_definition" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "import_statement" | "import_from_statement" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call")
            .then(|| call_target(node, "function", source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            return identifier_names(node, source)
                .into_iter()
                .rev()
                .take(1)
                .map(|target| Relation {
                    kind: "imports",
                    target,
                })
                .collect();
        }
        if node.kind() != "class_definition" {
            return Vec::new();
        }
        node.child_by_field_name("superclasses")
            .into_iter()
            .flat_map(|bases| identifier_names(bases, source))
            .map(|target| Relation {
                kind: "extends",
                target,
            })
            .collect()
    }
}
