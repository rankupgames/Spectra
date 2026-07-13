use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    LanguageAdapter, Relation, Scope, field_text, string_literal_value, terminal_identifier, text,
    truncate,
};

pub(crate) struct LuaAdapter;
pub(crate) struct LuauAdapter;
pub(crate) static LUA: LuaAdapter = LuaAdapter;
pub(crate) static LUAU: LuauAdapter = LuauAdapter;

impl LanguageAdapter for LuaAdapter {
    fn id(&self) -> &'static str {
        "lua"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_lua::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        classify_lua(node, false)
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        lua_label(node, source, mapped_kind)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        lua_call_name(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        lua_relations(node, source, "lua")
    }
}

impl LanguageAdapter for LuauAdapter {
    fn id(&self) -> &'static str {
        "luau"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["luau"]
    }

    fn language(&self, _path: &Path) -> Option<Language> {
        Some(tree_sitter_luau::LANGUAGE.into())
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        classify_lua(node, true)
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        lua_label(node, source, mapped_kind)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        lua_call_name(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        lua_relations(node, source, "luau")
    }
}

fn classify_lua(node: SyntaxNode<'_>, luau: bool) -> Option<&'static str> {
    match node.kind() {
        "function_declaration" => node
            .child_by_field_name("name")
            .map(|name| match name.kind() {
                "dot_index_expression" | "method_index_expression" => "method",
                _ => "function",
            }),
        "type_definition" if luau => Some("type_alias"),
        "function_call" => Some("import"),
        _ => None,
    }
}

fn lua_label(node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
    if mapped_kind == "import" {
        if !is_require(node, source) {
            return None;
        }
        return Some(truncate(text(node, source).trim(), 72));
    }
    node.child_by_field_name("name")
        .and_then(|name| terminal_identifier(name, source))
        .or_else(|| field_text(node, "name", source).map(str::to_owned))
}

fn lua_call_name(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "function_call" || is_require(node, source) {
        return None;
    }
    node.child_by_field_name("name")
        .and_then(|name| terminal_identifier(name, source))
}

fn lua_relations(node: SyntaxNode<'_>, source: &[u8], extension: &str) -> Vec<Relation> {
    if !is_require(node, source) {
        return Vec::new();
    }
    string_literal_value(node, source)
        .and_then(|path| {
            let file = path.rsplit('/').find(|part| !part.is_empty())?;
            if file.ends_with(&format!(".{extension}")) {
                Some(file.to_owned())
            } else {
                file.rsplit('.')
                    .find(|part| !part.is_empty())
                    .map(str::to_owned)
            }
        })
        .map(|mut target| {
            if !target.ends_with(extension) {
                target.push('.');
                target.push_str(extension);
            }
            vec![Relation {
                kind: "imports",
                target,
            }]
        })
        .unwrap_or_default()
}

fn is_require(node: SyntaxNode<'_>, source: &[u8]) -> bool {
    node.kind() == "function_call"
        && node
            .child_by_field_name("name")
            .and_then(|name| terminal_identifier(name, source))
            .is_some_and(|name| name == "require")
}
