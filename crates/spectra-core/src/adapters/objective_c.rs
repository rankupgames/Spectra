use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope,
    c_family::{c_family_call, c_family_label, include_relation},
    field_text, identifier_names, named_child_by_kind, terminal_identifier, text, truncate,
};

pub(crate) struct ObjectiveCAdapter;
pub(crate) static OBJECTIVE_C: ObjectiveCAdapter = ObjectiveCAdapter;

impl LanguageAdapter for ObjectiveCAdapter {
    fn id(&self) -> &'static str {
        "objective-c"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["m", "mm"]
    }

    fn language(&self, _path: &Path) -> Language {
        tree_sitter_objc::LANGUAGE.into()
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class_interface" | "class_declaration" => Some("class"),
            "class_implementation" => Some("impl"),
            "protocol_declaration" | "protocol_forward_declaration" => Some("interface"),
            "method_declaration" | "method_definition" => Some("method"),
            "function_definition" => Some("function"),
            "struct_specifier" | "union_specifier" => Some("struct"),
            "enum_specifier" => Some("enum"),
            "type_definition" | "compatibility_alias_declaration" => Some("type_alias"),
            "preproc_include" | "module_import" => Some("import"),
            _ => None,
        }
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        match mapped_kind {
            "import" => Some(truncate(text(node, source).trim(), 72)),
            "class" | "interface" | "impl" => type_label(node, source),
            "method" => identifier_names(node, source).into_iter().next(),
            _ => c_family_label(node, source, mapped_kind),
        }
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        if node.kind() == "message_expression" {
            return node
                .child_by_field_name("method")
                .and_then(|method| terminal_identifier(method, source));
        }
        c_family_call(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "preproc_include" {
            return include_relation(node, source);
        }
        if node.kind() == "module_import" {
            return terminal_identifier(node, source)
                .map(|target| {
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }

        let mut relations = Vec::new();
        if let Some(superclass) = field_text(node, "superclass", source) {
            relations.push(Relation {
                kind: "inherits",
                target: superclass.to_owned(),
            });
        }
        if let Some(protocols) = named_child_by_kind(node, "protocol_reference_list") {
            relations.extend(
                identifier_names(protocols, source)
                    .into_iter()
                    .map(|target| Relation {
                        kind: "inherits",
                        target,
                    }),
            );
        }
        if node.kind() == "class_interface"
            && let Some(protocols) = named_child_by_kind(node, "parameterized_arguments")
        {
            relations.extend(
                identifier_names(protocols, source)
                    .into_iter()
                    .map(|target| Relation {
                        kind: "inherits",
                        target,
                    }),
            );
        }
        if node.kind() == "class_implementation"
            && let Some(class) = direct_identifier(node, source)
        {
            relations.push(Relation {
                kind: "implements",
                target: class,
            });
        }
        relations
    }
}

fn type_label(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    let name = direct_identifier(node, source)?;
    if let Some(category) = field_text(node, "category", source) {
        Some(format!("{name}({category})"))
    } else {
        Some(name)
    }
}

fn direct_identifier(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == "identifier")
        .map(|identifier| text(identifier, source).to_owned())
}
