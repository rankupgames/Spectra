use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, call_target, field_text, identifier_names, inside_type,
    named_child_by_kind, string_literal_value, terminal_identifier, text, truncate,
};

pub(crate) struct CAdapter;
pub(crate) struct CppAdapter;
pub(crate) static C: CAdapter = CAdapter;
pub(crate) static CPP: CppAdapter = CppAdapter;

impl LanguageAdapter for CAdapter {
    fn id(&self) -> &'static str {
        "c"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["c", "h"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_c::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        classify_c_family(node, false)
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        c_family_label(node, source, mapped_kind)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        c_family_call(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        include_relation(node, source)
    }
}

impl LanguageAdapter for CppAdapter {
    fn id(&self) -> &'static str {
        "cpp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        if node.kind() == "function_definition" {
            return Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            });
        }
        classify_c_family(node, true)
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        c_family_label(node, source, mapped_kind)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        c_family_call(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        let mut relations = include_relation(node, source);
        if let Some(bases) = named_child_by_kind(node, "base_class_clause") {
            relations.extend(
                identifier_names(bases, source)
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

fn classify_c_family(node: SyntaxNode<'_>, cpp: bool) -> Option<&'static str> {
    match node.kind() {
        "function_definition" => Some("function"),
        "class_specifier" if cpp => Some("class"),
        "struct_specifier" | "union_specifier" => Some("struct"),
        "enum_specifier" => Some("enum"),
        "type_definition" | "alias_declaration" => Some("type_alias"),
        "namespace_definition" if cpp => Some("module"),
        "preproc_include" => Some("import"),
        _ => None,
    }
}

fn c_family_label(node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
    if mapped_kind == "import" {
        return Some(truncate(text(node, source).trim(), 72));
    }
    if matches!(mapped_kind, "function" | "method" | "type_alias") {
        return node
            .child_by_field_name("declarator")
            .and_then(|declarator| declarator_name(declarator, source));
    }
    field_text(node, "name", source).map(str::to_owned)
}

fn declarator_name(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "type_identifier" | "operator_name"
    ) {
        return Some(text(node, source).trim().to_owned());
    }
    if node.kind() == "qualified_identifier" {
        return terminal_identifier(node, source);
    }
    if let Some(inner) = node.child_by_field_name("declarator") {
        return declarator_name(inner, source);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() != "parameter_list")
        .find_map(|child| declarator_name(child, source))
}

fn c_family_call(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    (node.kind() == "call_expression")
        .then(|| call_target(node, "function", source))
        .flatten()
}

fn include_relation(node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
    if node.kind() != "preproc_include" {
        return Vec::new();
    }
    let target = string_literal_value(node, source).or_else(|| {
        let raw = text(node, source).trim();
        raw.strip_prefix("#include")
            .map(str::trim)
            .map(|value| value.trim_matches(['\'', '"', '<', '>']).to_owned())
    });
    target
        .and_then(|target| target.rsplit('/').next().map(str::to_owned))
        .filter(|target| !target.is_empty())
        .map(|target| {
            vec![Relation {
                kind: "imports",
                target,
            }]
        })
        .unwrap_or_default()
}
