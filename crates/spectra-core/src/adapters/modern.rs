use std::{collections::BTreeSet, path::Path};

use tree_sitter::{Language, Node as SyntaxNode};

use super::{
    FileSymbol, LanguageAdapter, Relation, Scope, TYPESCRIPT, file_symbol, quoted_values, relation,
};

pub(crate) struct RAdapter;
pub(crate) struct ErlangAdapter;
pub(crate) struct SolidityAdapter;
pub(crate) struct PascalAdapter;
pub(crate) struct ArkTsAdapter;
pub(crate) struct RazorAdapter;
pub(crate) struct VbNetAdapter;

pub(crate) static R: RAdapter = RAdapter;
pub(crate) static ERLANG: ErlangAdapter = ErlangAdapter;
pub(crate) static SOLIDITY: SolidityAdapter = SolidityAdapter;
pub(crate) static PASCAL: PascalAdapter = PascalAdapter;
pub(crate) static ARKTS: ArkTsAdapter = ArkTsAdapter;
pub(crate) static RAZOR: RazorAdapter = RazorAdapter;
pub(crate) static VBNET: VbNetAdapter = VbNetAdapter;

macro_rules! custom_adapter {
    ($adapter:ty, $id:literal, $extensions:expr, $extractor:ident) => {
        impl LanguageAdapter for $adapter {
            fn id(&self) -> &'static str {
                $id
            }

            fn extensions(&self) -> &'static [&'static str] {
                $extensions
            }

            fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
                $extractor(path, source)
            }
        }
    };
}

custom_adapter!(RAdapter, "r", &["r", "R"], r_symbols);
custom_adapter!(SolidityAdapter, "solidity", &["sol"], solidity_symbols);
custom_adapter!(
    PascalAdapter,
    "pascal",
    &["pas", "dpr", "dpk", "lpr", "dfm", "fmx"],
    pascal_symbols
);
custom_adapter!(RazorAdapter, "razor", &["cshtml", "razor"], razor_symbols);
custom_adapter!(VbNetAdapter, "vbnet", &["vb"], vbnet_symbols);

impl LanguageAdapter for ErlangAdapter {
    fn id(&self) -> &'static str {
        "erlang"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["erl", "hrl", "escript", "app"]
    }

    fn matches_path(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".app.src"))
            || path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| self.extensions().contains(&extension))
    }

    fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
        erlang_symbols(path, source)
    }
}

impl LanguageAdapter for ArkTsAdapter {
    fn id(&self) -> &'static str {
        "arkts"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ets"]
    }

    fn language(&self, path: &Path) -> Option<Language> {
        TYPESCRIPT.language(path)
    }

    fn classify(
        &self,
        node: SyntaxNode<'_>,
        source: &[u8],
        scopes: &[Scope],
    ) -> Option<&'static str> {
        TYPESCRIPT.classify(node, source, scopes)
    }

    fn label(&self, node: SyntaxNode<'_>, source: &[u8], mapped_kind: &str) -> Option<String> {
        TYPESCRIPT.label(node, source, mapped_kind)
    }

    fn call_name(&self, node: SyntaxNode<'_>, source: &[u8]) -> Option<String> {
        TYPESCRIPT.call_name(node, source)
    }

    fn relations(&self, node: SyntaxNode<'_>, source: &[u8]) -> Vec<Relation> {
        TYPESCRIPT.relations(node, source)
    }

    fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
        arkts_symbols(path, source)
    }
}

fn r_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let calls = call_names(
        source,
        &["function", "if", "for", "while", "library", "require"],
    );
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if let Some((name, value)) = line.split_once("<-").or_else(|| line.split_once('='))
            && value.trim_start().starts_with("function")
        {
            symbols.push(file_symbol(
                "function",
                name.trim(),
                offset + 1,
                calls
                    .iter()
                    .cloned()
                    .map(|target| relation("calls", target))
                    .collect(),
            ));
        }
        for (marker, kind) in [
            ("setClass(", "class"),
            ("setGeneric(", "function"),
            ("setMethod(", "method"),
        ] {
            if line.contains(marker)
                && let Some(name) = quoted_values(line).first()
            {
                symbols.push(file_symbol(kind, name, offset + 1, Vec::new()));
            }
        }
        for marker in ["library(", "require("] {
            if let Some(rest) = line.split_once(marker).map(|(_, rest)| rest)
                && let Some(target) = rest
                    .split(')')
                    .next()
                    .map(|value| value.trim_matches(['\'', '"']))
                && !target.is_empty()
            {
                symbols.push(file_symbol(
                    "import",
                    target,
                    offset + 1,
                    vec![relation("imports", target)],
                ));
            }
        }
    }
    symbols
}

fn erlang_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    let calls = erlang_calls(source);
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if let Some(name) = directive_value(line, "-module(") {
            symbols.push(file_symbol("module", name, offset + 1, Vec::new()));
        }
        if let Some(target) =
            directive_value(line, "-behaviour(").or_else(|| directive_value(line, "-behavior("))
        {
            symbols.push(file_symbol(
                "interface",
                target.clone(),
                offset + 1,
                vec![relation("implements", target)],
            ));
        }
        if line.starts_with("-include")
            && let Some(target) = quoted_values(line).first()
        {
            symbols.push(file_symbol(
                "import",
                target,
                offset + 1,
                vec![relation("imports", target)],
            ));
        }
        if let Some((head, _)) = line.split_once("->")
            && let Some(open) = head.find('(')
        {
            let name = head[..open].trim();
            if is_name(name) {
                symbols.push(file_symbol(
                    "function",
                    name,
                    offset + 1,
                    calls
                        .iter()
                        .cloned()
                        .map(|target| relation("calls", target))
                        .collect(),
                ));
            }
        }
    }
    if symbols.is_empty()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".app.src"))
    {
        symbols.push(file_symbol(
            "module",
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("application"),
            1,
            Vec::new(),
        ));
    }
    symbols
}

fn directive_value(line: &str, marker: &str) -> Option<String> {
    let rest = line.strip_prefix(marker)?;
    let value = rest.split(')').next()?.trim_matches(['\'', '"']);
    (!value.is_empty()).then(|| value.to_owned())
}

fn erlang_calls(source: &str) -> BTreeSet<String> {
    let mut calls = call_names(source, &["if", "case", "receive", "fun"]);
    for token in source.split_whitespace() {
        if let Some((_, function)) = token.split_once(':') {
            let name = function.split('(').next().unwrap_or("");
            if is_name(name) {
                calls.insert(name.to_owned());
            }
        }
    }
    calls
}

fn solidity_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let calls = call_names(
        source,
        &["if", "for", "while", "require", "assert", "revert"],
    );
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        for (marker, kind) in [
            ("contract ", "class"),
            ("interface ", "interface"),
            ("library ", "module"),
            ("struct ", "struct"),
            ("enum ", "enum"),
            ("event ", "event"),
            ("modifier ", "method"),
            ("function ", "method"),
        ] {
            let Some(marker_index) = line.find(marker) else {
                continue;
            };
            let rest = &line[marker_index + marker.len()..];
            let name = rest
                .split(|character: char| {
                    character.is_whitespace() || character == '(' || character == '{'
                })
                .next()
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let mut relations = Vec::new();
            if matches!(marker, "contract " | "interface ")
                && let Some((_, bases)) = rest.split_once(" is ")
            {
                relations.extend(
                    bases
                        .split(['{', ','])
                        .map(str::trim)
                        .filter(|base| is_name(base))
                        .map(|base| relation("inherits", base)),
                );
            }
            if matches!(kind, "method") {
                relations.extend(
                    calls
                        .iter()
                        .cloned()
                        .map(|target| relation("calls", target)),
                );
            }
            symbols.push(file_symbol(kind, name, offset + 1, relations));
        }
        if let Some(target) = line
            .strip_prefix("import ")
            .and_then(|line| quoted_values(line).first().cloned())
        {
            symbols.push(file_symbol(
                "import",
                target.clone(),
                offset + 1,
                vec![relation("imports", target)],
            ));
        }
    }
    symbols
}

fn pascal_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let calls = call_names(source, &["if", "while", "for", "case", "with"]);
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        for (marker, kind) in [
            ("unit ", "module"),
            ("program ", "module"),
            ("library ", "module"),
            ("package ", "module"),
            ("procedure ", "function"),
            ("function ", "function"),
        ] {
            if let Some(rest) = lower.strip_prefix(marker) {
                let length = rest.split(['(', ':', ';', ' ']).next().unwrap_or("").len();
                let start = marker.len();
                let name = line.get(start..start + length).unwrap_or("");
                if !name.is_empty() {
                    let relations = if kind == "function" {
                        calls
                            .iter()
                            .cloned()
                            .map(|target| relation("calls", target))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    symbols.push(file_symbol(kind, name, offset + 1, relations));
                }
            }
        }
        if lower.starts_with("uses ") {
            for target in line[5..]
                .split([',', ';'])
                .map(str::trim)
                .filter(|target| !target.is_empty())
            {
                symbols.push(file_symbol(
                    "import",
                    target,
                    offset + 1,
                    vec![relation("imports", target)],
                ));
            }
        }
        if let Some((name, value)) = line.split_once('=')
            && value.trim_start().to_ascii_lowercase().starts_with("class")
        {
            let relations = value
                .split_once('(')
                .and_then(|(_, rest)| rest.split_once(')'))
                .map(|(bases, _)| {
                    bases
                        .split(',')
                        .map(str::trim)
                        .filter(|base| !base.is_empty())
                        .map(|base| relation("inherits", base))
                        .collect()
                })
                .unwrap_or_default();
            let name = name.trim().strip_prefix("type ").unwrap_or(name.trim());
            symbols.push(file_symbol("class", name, offset + 1, relations));
        }
    }
    symbols
}

fn arkts_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("struct ") {
            let name = rest
                .split(|character: char| character.is_whitespace() || character == '{')
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                symbols.push(file_symbol("component", name, offset + 1, Vec::new()));
            }
        }
        if line.contains("pushUrl") || line.contains("replaceUrl") {
            for target in quoted_values(line) {
                if target.starts_with('/') || target.contains("pages/") {
                    symbols.push(file_symbol("route", target, offset + 1, Vec::new()));
                }
            }
        }
    }
    if symbols.is_empty() && source.contains("@Component") {
        symbols.push(file_symbol(
            "component",
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("component"),
            1,
            Vec::new(),
        ));
    }
    symbols
}

fn razor_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let component = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("component");
    let mut relations = BTreeSet::new();
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if let Some(route) = line
            .strip_prefix("@page ")
            .and_then(|value| quoted_values(value).first().cloned())
        {
            symbols.push(file_symbol("route", route, offset + 1, Vec::new()));
        }
        for target in markup_components(line) {
            relations.insert(("renders", target));
        }
        for marker in ["@onclick=", "@onchange=", "@onsubmit="] {
            if let Some(rest) = line.split_once(marker).map(|(_, rest)| rest)
                && let Some(target) = quoted_values(rest).first()
            {
                relations.insert(("binds", target.clone()));
            }
        }
        if let Some(name) = csharp_method_name(line) {
            symbols.push(file_symbol("method", name, offset + 1, Vec::new()));
        }
    }
    symbols.insert(
        0,
        file_symbol(
            "component",
            component,
            1,
            relations
                .into_iter()
                .map(|(kind, target)| relation(kind, target))
                .collect(),
        ),
    );
    symbols
}

fn markup_components(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let bytes = line.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'<' && bytes[index + 1].is_ascii_uppercase() {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if let Some(value) = line.get(start..end).filter(|value| !value.is_empty()) {
                values.push(value.to_owned());
            }
            index = end;
        } else {
            index += 1;
        }
    }
    values
}

fn csharp_method_name(line: &str) -> Option<String> {
    let open = line.find('(')?;
    let before = line[..open].trim_end();
    let name = before.split_whitespace().last()?;
    let return_type = before.split_whitespace().rev().nth(1)?;
    (matches!(return_type, "void" | "Task" | "string" | "int" | "bool") && is_name(name))
        .then(|| name.to_owned())
}

fn vbnet_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let calls = call_names(source, &["If", "For", "While", "Sub", "Function", "New"]);
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('\'') || line.is_empty() {
            continue;
        }
        let words = line.split_whitespace().collect::<Vec<_>>();
        let declaration = words.iter().position(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "namespace"
                    | "module"
                    | "class"
                    | "interface"
                    | "structure"
                    | "enum"
                    | "sub"
                    | "function"
            )
        });
        if let Some(index) = declaration
            && let Some(name) = words
                .get(index + 1)
                .map(|word| word.split('(').next().unwrap_or(word))
        {
            let kind = match words[index].to_ascii_lowercase().as_str() {
                "namespace" | "module" => "module",
                "class" => "class",
                "interface" => "interface",
                "structure" => "struct",
                "enum" => "enum",
                "sub" | "function" => "method",
                _ => continue,
            };
            let relations = if kind == "method" {
                calls
                    .iter()
                    .cloned()
                    .map(|target| relation("calls", target))
                    .collect()
            } else {
                Vec::new()
            };
            symbols.push(file_symbol(kind, name, offset + 1, relations));
        }
        for (marker, kind) in [
            ("Imports ", "imports"),
            ("Inherits ", "inherits"),
            ("Implements ", "implements"),
        ] {
            if let Some(targets) = line.strip_prefix(marker) {
                for target in targets
                    .split(',')
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                {
                    symbols.push(file_symbol(
                        "reference",
                        target,
                        offset + 1,
                        vec![relation(kind, target)],
                    ));
                }
            }
        }
    }
    symbols
}

fn call_names(source: &str, excluded: &[&str]) -> BTreeSet<String> {
    let mut calls = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !(bytes[index].is_ascii_alphabetic() || matches!(bytes[index], b'_' | b'$')) {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
        {
            index += 1;
        }
        let name = &source[start..index];
        let mut next = index;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        if next < bytes.len()
            && bytes[next] == b'('
            && !excluded
                .iter()
                .any(|value| value.eq_ignore_ascii_case(name))
        {
            calls.insert(name.to_owned());
        }
    }
    calls
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
}
