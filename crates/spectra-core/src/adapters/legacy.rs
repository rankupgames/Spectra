use std::{collections::BTreeSet, path::Path};

use super::{FileSymbol, LanguageAdapter, file_symbol, quoted_values, relation};

pub(crate) struct CfmlAdapter;
pub(crate) struct CobolAdapter;

pub(crate) static CFML: CfmlAdapter = CfmlAdapter;
pub(crate) static COBOL: CobolAdapter = CobolAdapter;

impl LanguageAdapter for CfmlAdapter {
    fn id(&self) -> &'static str {
        "cfml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cfc", "cfm", "cfs"]
    }

    fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
        cfml_symbols(path, source)
    }
}

impl LanguageAdapter for CobolAdapter {
    fn id(&self) -> &'static str {
        "cobol"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cbl", "cob", "cobol", "cpy"]
    }

    fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
        cobol_symbols(path, source)
    }
}

fn cfml_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let component = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("component");
    let mut symbols = Vec::new();
    let calls = cfml_calls(source);
    if path.extension().is_some_and(|extension| extension == "cfc")
        || source.to_ascii_lowercase().contains("component")
    {
        symbols.push(file_symbol("class", component, 1, Vec::new()));
    }
    let mut in_query = false;
    let mut query_start = 1;
    let mut query_name = String::new();
    for (offset, raw) in source.lines().enumerate() {
        let line_number = offset + 1;
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("function ")
            .or_else(|| lower.split_once(" function ").map(|(_, rest)| rest))
        {
            let length = rest.split('(').next().unwrap_or("").trim().len();
            let position = lower.find(rest).unwrap_or(0);
            let name = line.get(position..position + length).unwrap_or("");
            if !name.is_empty() {
                symbols.push(file_symbol(
                    "method",
                    name,
                    line_number,
                    calls
                        .iter()
                        .cloned()
                        .map(|target| relation("calls", target))
                        .collect(),
                ));
            }
        }
        if lower.starts_with("<cffunction")
            && let Some(name) = attribute(line, "name")
        {
            symbols.push(file_symbol(
                "method",
                name,
                line_number,
                calls
                    .iter()
                    .cloned()
                    .map(|target| relation("calls", target))
                    .collect(),
            ));
        }
        if (lower.starts_with("<cfinclude") || lower.starts_with("<cfmodule"))
            && let Some(target) = attribute(line, "template")
        {
            symbols.push(file_symbol(
                "import",
                target.clone(),
                line_number,
                vec![relation("imports", target)],
            ));
        }
        if lower.starts_with("include ")
            && let Some(target) = quoted_values(line).first()
        {
            symbols.push(file_symbol(
                "import",
                target,
                line_number,
                vec![relation("imports", target)],
            ));
        }
        if lower.contains("<cfquery") {
            in_query = true;
            query_start = line_number;
            query_name = attribute(line, "name").unwrap_or_else(|| format!("query@{line_number}"));
        }
        if in_query && lower.contains("</cfquery>") {
            symbols.push(FileSymbol {
                kind: "query",
                label: query_name.clone(),
                start_line: query_start as u32,
                end_line: line_number as u32,
                relations: Vec::new(),
            });
            in_query = false;
        }
        if let Some((name, value)) = line.split_once('=')
            && value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("queryexecute(")
        {
            symbols.push(file_symbol("query", name.trim(), line_number, Vec::new()));
        }
    }
    symbols
}

fn attribute(line: &str, name: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker = format!("{name}=");
    let index = lower.find(&marker)? + marker.len();
    let rest = line[index..].trim_start();
    let quote = rest.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    Some(rest[..rest.find(quote)?].to_owned())
}

fn cfml_calls(source: &str) -> BTreeSet<String> {
    let mut calls = BTreeSet::new();
    for token in source.split(|character: char| {
        character.is_whitespace() || matches!(character, ';' | '{' | '}' | '=')
    }) {
        let Some((name, _)) = token.split_once('(') else {
            continue;
        };
        let name = name.rsplit('.').next().unwrap_or(name);
        if !name.is_empty()
            && name
                .chars()
                .all(|character| character == '_' || character.is_alphanumeric())
            && !matches!(
                name.to_ascii_lowercase().as_str(),
                "if" | "for" | "while" | "function" | "queryexecute"
            )
        {
            calls.insert(name.to_owned());
        }
    }
    calls
}

fn cobol_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    let mut program = None;
    let calls = cobol_relations(source);
    for (offset, raw) in source.lines().enumerate() {
        let line_number = offset + 1;
        let line = raw.trim();
        let upper = line.to_ascii_uppercase();
        if let Some(rest) = upper.split_once("PROGRAM-ID.").map(|(_, rest)| rest) {
            let length = rest.trim().trim_end_matches('.').len();
            let position = upper.find(rest).unwrap_or(0) + rest.len() - rest.trim_start().len();
            let label = line.get(position..position + length).unwrap_or(rest.trim());
            program = Some(label.to_owned());
            symbols.push(file_symbol(
                "module",
                label,
                line_number,
                calls
                    .iter()
                    .cloned()
                    .map(|(kind, target)| relation(kind, target))
                    .collect(),
            ));
        }
        if let Some(rest) = upper.strip_prefix("COPY ") {
            let length = rest.trim_end_matches('.').trim().len();
            let target = line.get(5..5 + length).unwrap_or(rest.trim()).to_owned();
            symbols.push(file_symbol(
                "import",
                target.clone(),
                line_number,
                vec![relation("imports", target)],
            ));
        }
        if (upper.ends_with(" SECTION.") || is_paragraph(&upper)) && !upper.contains("PROGRAM-ID") {
            let label = line
                .trim_end_matches('.')
                .trim_end_matches(" SECTION")
                .trim_end_matches(" section")
                .trim();
            if !label.is_empty() {
                symbols.push(file_symbol("function", label, line_number, Vec::new()));
            }
        }
    }
    if symbols.is_empty() {
        symbols.push(file_symbol(
            "module",
            program.unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("program")
                    .to_owned()
            }),
            1,
            calls
                .into_iter()
                .map(|(kind, target)| relation(kind, target))
                .collect(),
        ));
    }
    symbols
}

fn cobol_relations(source: &str) -> BTreeSet<(&'static str, String)> {
    let mut relations = BTreeSet::new();
    for raw in source.lines() {
        let line = raw.trim();
        let upper = line.to_ascii_uppercase();
        for marker in ["CALL ", "PERFORM "] {
            if let Some(rest) = upper.split_once(marker).map(|(_, rest)| rest) {
                let target = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(['\'', '"', '.']);
                if !target.is_empty() {
                    relations.insert(("calls", target.to_owned()));
                }
            }
        }
        if upper.contains("EXEC CICS")
            && upper.contains("LINK")
            && let Some(rest) = upper.split_once("PROGRAM(").map(|(_, rest)| rest)
        {
            let target = rest
                .split(')')
                .next()
                .unwrap_or("")
                .trim_matches(['\'', '"', ' ']);
            if !target.is_empty() {
                relations.insert(("calls", target.to_owned()));
            }
        }
    }
    relations
}

fn is_paragraph(line: &str) -> bool {
    line.ends_with('.')
        && !line.contains(char::is_whitespace)
        && line
            .trim_end_matches('.')
            .chars()
            .all(|character| character == '-' || character == '_' || character.is_alphanumeric())
}
