use std::path::Path;

use tree_sitter::{Language, Node as SyntaxNode};

mod c_family;
mod csharp;
mod dart;
mod frameworks;
mod go;
mod java;
mod javascript;
mod kotlin;
mod legacy;
mod lua_family;
mod modern;
mod objective_c;
mod php;
mod python;
mod ruby;
mod rust;
mod scala;
mod structured;
mod swift;
mod typescript;
mod web;

pub(crate) use go::GO;
pub(crate) use java::JAVA;
pub(crate) use javascript::JAVASCRIPT;
pub(crate) use kotlin::KOTLIN;
pub(crate) use lua_family::{LUA, LUAU};
pub(crate) use objective_c::OBJECTIVE_C;
pub(crate) use python::PYTHON;
pub(crate) use rust::RUST;
pub(crate) use scala::SCALA;
pub(crate) use swift::SWIFT;
pub(crate) use typescript::TYPESCRIPT;
pub(crate) use web::{ASTRO, LIQUID, SVELTE, VUE};

static ADAPTERS: [&dyn LanguageAdapter; 39] = [
    &RUST,
    &TYPESCRIPT,
    &JAVASCRIPT,
    &PYTHON,
    &GO,
    &JAVA,
    &C,
    &CPP,
    &CSHARP,
    &PHP,
    &RUBY,
    &SWIFT,
    &KOTLIN,
    &SCALA,
    &DART,
    &LUA,
    &LUAU,
    &SVELTE,
    &VUE,
    &ASTRO,
    &LIQUID,
    &OBJECTIVE_C,
    &CUDA,
    &METAL,
    &R,
    &NIX,
    &ERLANG,
    &SOLIDITY,
    &TERRAFORM,
    &PASCAL,
    &ARKTS,
    &RAZOR,
    &VBNET,
    &CFML,
    &COBOL,
    &YAML,
    &TWIG,
    &XML,
    &PROPERTIES,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupportedLanguage {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Scope {
    pub(crate) kind: &'static str,
    pub(crate) label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Relation {
    pub(crate) kind: &'static str,
    pub(crate) target: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileSymbol {
    pub(crate) kind: &'static str,
    pub(crate) label: String,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    pub(crate) relations: Vec<Relation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EmbeddedRegion {
    pub(crate) language: &'static str,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

pub(crate) trait LanguageAdapter: Sync {
    fn id(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn matches_path(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| self.extensions().contains(&extension))
    }
    fn language(&self, _path: &Path) -> Option<Language> {
        None
    }
    fn classify(
        &self,
        _node: SyntaxNode<'_>,
        _source: &[u8],
        _scopes: &[Scope],
    ) -> Option<&'static str> {
        None
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        if mapped_kind == "import" {
            return Some(truncate(
                text(node, source).trim().trim_end_matches(';'),
                72,
            ));
        }
        node.child_by_field_name("name")
            .map(|name| text(name, source).to_owned())
            .filter(|name| !name.is_empty())
    }

    fn call_name(&self, _node: SyntaxNode<'_>, _source: &[u8]) -> Option<String> {
        None
    }

    fn call_kind(&self, _node: SyntaxNode<'_>) -> &'static str {
        "calls"
    }

    fn relations(&self, _node: SyntaxNode<'_>, _source: &[u8]) -> Vec<Relation> {
        Vec::new()
    }

    fn file_symbols(&self, _path: &Path, _source: &str) -> Vec<FileSymbol> {
        Vec::new()
    }

    fn embedded_regions(&self, _source: &str) -> Vec<EmbeddedRegion> {
        Vec::new()
    }

    fn opens_scope(&self, kind: &str) -> bool {
        matches!(
            kind,
            "module"
                | "class"
                | "struct"
                | "enum"
                | "interface"
                | "trait"
                | "impl"
                | "function"
                | "method"
                | "kernel"
        )
    }
}

pub(crate) fn for_path(path: &Path) -> Option<&'static dyn LanguageAdapter> {
    ADAPTERS
        .iter()
        .copied()
        .find(|adapter| adapter.matches_path(path))
}

pub fn is_supported_path(path: &Path) -> bool {
    for_path(path).is_some()
}

pub(crate) fn for_id(id: &str) -> Option<&'static dyn LanguageAdapter> {
    ADAPTERS.iter().copied().find(|adapter| adapter.id() == id)
}

pub fn supported_languages() -> Vec<SupportedLanguage> {
    ADAPTERS
        .iter()
        .map(|adapter| SupportedLanguage {
            id: adapter.id(),
            extensions: adapter.extensions(),
        })
        .collect()
}

pub(crate) fn text<'a>(node: SyntaxNode<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or("")
}

pub(crate) fn field_text<'a>(
    node: SyntaxNode<'_>,
    field: &str,
    source: &'a [u8],
) -> Option<&'a str> {
    node.child_by_field_name(field)
        .map(|child| text(child, source).trim())
        .filter(|value| !value.is_empty())
}

pub(crate) fn call_target(node: SyntaxNode<'_>, field: &str, source: &[u8]) -> Option<String> {
    let target = node.child_by_field_name(field)?;
    terminal_identifier(target, source)
}

pub(crate) fn leading_identifier(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| terminal_identifier(child, source))
}

pub(crate) fn descendant_field_text<'a>(
    node: SyntaxNode<'_>,
    field: &str,
    source: &'a [u8],
) -> Option<&'a str> {
    if let Some(value) = field_text(node, field, source) {
        return Some(value);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| descendant_field_text(child, field, source))
}

pub(crate) fn terminal_identifier(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier"
            | "field_identifier"
            | "property_identifier"
            | "type_identifier"
            | "simple_identifier"
            | "name"
            | "constant"
            | "method_name"
            | "variable_name"
    ) {
        let value = text(node, source).trim();
        return (!value.is_empty()).then(|| value.to_owned());
    }
    let mut cursor = node.walk();
    let mut terminal = None;
    for child in node.named_children(&mut cursor) {
        if let Some(identifier) = terminal_identifier(child, source) {
            terminal = Some(identifier);
        }
    }
    terminal
}

pub(crate) fn identifier_names(node: SyntaxNode<'_>, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    collect_identifier_names(node, source, &mut names);
    names
}

pub(crate) fn named_child_by_kind<'tree>(
    node: SyntaxNode<'tree>,
    kind: &str,
) -> Option<SyntaxNode<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn collect_identifier_names(node: SyntaxNode<'_>, source: &[u8], names: &mut Vec<String>) {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "simple_identifier"
            | "name"
            | "constant"
            | "method_name"
    ) {
        let value = text(node, source).trim();
        if !value.is_empty() {
            names.push(value.to_owned());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifier_names(child, source, names);
    }
}

pub(crate) fn string_literal_value(node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "string_literal" | "string" | "interpreted_string_literal"
    ) {
        let value = text(node, source)
            .trim()
            .trim_matches(['\'', '"', '<', '>']);
        return (!value.is_empty()).then(|| value.to_owned());
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| string_literal_value(child, source))
}

pub(crate) fn inside_type(scopes: &[Scope]) -> bool {
    scopes
        .iter()
        .rev()
        .take_while(|scope| !matches!(scope.kind, "function" | "method" | "kernel"))
        .any(|scope| {
            matches!(
                scope.kind,
                "class" | "struct" | "interface" | "trait" | "impl"
            )
        })
}

pub(crate) fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    }
}
pub(crate) fn file_symbol(
    kind: &'static str,
    label: impl Into<String>,
    line: usize,
    relations: Vec<Relation>,
) -> FileSymbol {
    FileSymbol {
        kind,
        label: label.into(),
        start_line: line as u32,
        end_line: line as u32,
        relations,
    }
}

pub(crate) fn relation(kind: &'static str, target: impl Into<String>) -> Relation {
    Relation {
        kind,
        target: target.into(),
    }
}

pub(crate) fn quoted_values(source: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        match quote {
            Some(open) if character == open => {
                if let Some(value) = source.get(start..index).filter(|value| !value.is_empty()) {
                    values.push(value.to_owned());
                }
                quote = None;
            }
            None if matches!(character, '\'' | '"') => {
                quote = Some(character);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    values
}

pub(crate) use c_family::{C, CPP, CUDA, METAL};
pub(crate) use csharp::CSHARP;
pub(crate) use dart::DART;
pub(crate) use legacy::{CFML, COBOL};
pub(crate) use modern::{ARKTS, ERLANG, PASCAL, R, RAZOR, SOLIDITY, VBNET};
pub(crate) use php::PHP;
pub(crate) use ruby::RUBY;
pub(crate) use structured::{NIX, PROPERTIES, TERRAFORM, TWIG, XML, YAML};
