use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, field_text, inside_type, string_literal_value,
    terminal_identifier, text, truncate,
};

pub(crate) struct RubyAdapter;
pub(crate) static RUBY: RubyAdapter = RubyAdapter;

impl LanguageAdapter for RubyAdapter {
    fn id(&self) -> &'static str {
        "ruby"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rb"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_ruby::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        match node.kind() {
            "class" => Some("class"),
            "module" => Some("module"),
            "method" => Some(if inside_type(scopes) {
                "method"
            } else {
                "function"
            }),
            "singleton_method" => Some("method"),
            "call" => Some("import"),
            _ => None,
        }
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        if mapped_kind == "import" {
            let method = field_text(node, "method", source)?;
            if !matches!(method, "require" | "require_relative" | "load") {
                return None;
            }
            return Some(truncate(text(node, source).trim(), 72));
        }
        field_text(node, "name", source).map(str::to_owned)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        if node.kind() != "call" {
            return None;
        }
        let name = field_text(node, "method", source)?;
        (!matches!(name, "require" | "require_relative" | "load")).then(|| name.to_owned())
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        if node.kind() == "call"
            && field_text(node, "method", source)
                .is_some_and(|name| matches!(name, "require" | "require_relative" | "load"))
        {
            return string_literal_value(node, source)
                .and_then(|path| path.rsplit('/').next().map(str::to_owned))
                .map(|mut target| {
                    if !target.contains('.') {
                        target.push_str(".rb");
                    }
                    vec![Relation {
                        kind: "imports",
                        target,
                    }]
                })
                .unwrap_or_default();
        }
        if node.kind() != "class" {
            return Vec::new();
        }
        node.child_by_field_name("superclass")
            .and_then(|superclass| terminal_identifier(superclass, source))
            .map(|target| {
                vec![Relation {
                    kind: "inherits",
                    target,
                }]
            })
            .unwrap_or_default()
    }
}
