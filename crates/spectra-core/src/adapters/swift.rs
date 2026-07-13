use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    FileSymbol, LanguageAdapter, Relation, Scope, field_text, frameworks, identifier_names,
    inside_type, leading_identifier, terminal_identifier,
};

pub(crate) struct SwiftAdapter;
pub(crate) static SWIFT: SwiftAdapter = SwiftAdapter;

impl LanguageAdapter for SwiftAdapter {
    fn id(&self) -> &'static str {
        "swift"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["swift"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_swift::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_declaration" => match node
                .child_by_field_name("declaration_kind")
                .map(|kind| kind.kind())
            {
                Some("struct") => Some("struct"),
                Some("enum") => Some("enum"),
                Some("extension") => Some("impl"),
                _ => Some("class"),
            },
            "protocol_declaration" => Some("interface"),
            "function_declaration" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "typealias_declaration" => Some("type_alias"),
            "import_declaration" => Some("import"),
            _ => None,
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        (node.kind() == "call_expression")
            .then(|| leading_identifier(node, source))
            .flatten()
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
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
        let mut cursor = node.walk();
        let mut relations: Vec<_> = node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "inheritance_specifier")
            .flat_map(|specifier| identifier_names(specifier, source))
            .map(|target| Relation {
                kind: "inherits",
                target,
            })
            .collect();
        if node
            .child_by_field_name("declaration_kind")
            .is_some_and(|kind| kind.kind() == "extension")
            && let Some(target) = field_text(node, "name", source)
        {
            relations.push(Relation {
                kind: "extends",
                target: target.to_owned(),
            });
        }
        relations
    }

    fn file_symbols(&self, _path: &Path, source: &str) -> Vec<FileSymbol> {
        let mut symbols = frameworks::swift_routes(source);
        symbols.extend(frameworks::swift_client_symbols(source));
        symbols
    }
}
