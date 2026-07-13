use std::{collections::BTreeSet, path::Path};

use super::{FileSymbol, LanguageAdapter, file_symbol, quoted_values, relation};

pub(crate) struct YamlAdapter;
pub(crate) struct TwigAdapter;
pub(crate) struct XmlAdapter;
pub(crate) struct PropertiesAdapter;
pub(crate) struct TerraformAdapter;
pub(crate) struct NixAdapter;

pub(crate) static YAML: YamlAdapter = YamlAdapter;
pub(crate) static TWIG: TwigAdapter = TwigAdapter;
pub(crate) static XML: XmlAdapter = XmlAdapter;
pub(crate) static PROPERTIES: PropertiesAdapter = PropertiesAdapter;
pub(crate) static TERRAFORM: TerraformAdapter = TerraformAdapter;
pub(crate) static NIX: NixAdapter = NixAdapter;

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

custom_adapter!(YamlAdapter, "yaml", &["yml", "yaml"], yaml_symbols);
custom_adapter!(TwigAdapter, "twig", &["twig"], twig_symbols);
custom_adapter!(XmlAdapter, "xml", &["xml"], xml_symbols);
custom_adapter!(
    PropertiesAdapter,
    "properties",
    &["properties"],
    properties_symbols
);
custom_adapter!(
    TerraformAdapter,
    "terraform",
    &["tf", "tfvars", "tofu"],
    terraform_symbols
);
custom_adapter!(NixAdapter, "nix", &["nix"], nix_symbols);

fn yaml_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut route: Option<(usize, usize)> = None;
    for (offset, raw) in source.lines().enumerate() {
        let line_number = offset + 1;
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let entry = trimmed.trim_start_matches("- ");
        let Some((raw_key, raw_value)) = entry.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().trim_matches(['\'', '"']);
        if key.is_empty() || key.contains(" ") {
            continue;
        }
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        let value = raw_value.trim().trim_matches(['\'', '"']);
        let mut path = stack
            .iter()
            .map(|(_, key)| key.as_str())
            .collect::<Vec<_>>();
        path.push(key);
        let qualified = path.join(".");

        if indent == 0 && value.is_empty() && key.contains('.') {
            symbols.push(file_symbol("route", key, line_number, Vec::new()));
            route = Some((symbols.len() - 1, indent));
        } else if let Some((index, route_indent)) = route
            && indent > route_indent
            && matches!(key, "_controller" | "controller")
            && !value.is_empty()
        {
            let target = value
                .trim_start_matches('\\')
                .split("::")
                .last()
                .unwrap_or(value)
                .to_owned();
            symbols[index].relations.push(relation("routes_to", target));
        }

        if value.is_empty() {
            stack.push((indent, key.to_owned()));
        } else {
            let relations = placeholder_references(value)
                .into_iter()
                .map(|target| relation("references", target))
                .collect();
            symbols.push(file_symbol("constant", qualified, line_number, relations));
        }
    }
    symbols
}

fn properties_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    source
        .lines()
        .enumerate()
        .filter_map(|(offset, raw)| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(['#', '!']) {
                return None;
            }
            let (key, value) = line.split_once('=').or_else(|| line.split_once(':'))?;
            let key = key.trim();
            (!key.is_empty()).then(|| {
                file_symbol(
                    "constant",
                    key,
                    offset + 1,
                    placeholder_references(value)
                        .into_iter()
                        .map(|target| relation("references", target))
                        .collect(),
                )
            })
        })
        .collect()
}

fn placeholder_references(value: &str) -> Vec<String> {
    let mut references = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find("${") {
        let start = cursor + relative + 2;
        let Some(end) = value[start..].find('}') else {
            break;
        };
        let target = value[start..start + end]
            .split(':')
            .next()
            .unwrap_or("")
            .trim();
        if !target.is_empty() {
            references.push(target.to_owned());
        }
        cursor = start + end + 1;
    }
    references
}

fn twig_symbols(path: &Path, source: &str) -> Vec<FileSymbol> {
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("template");
    let mut relations = BTreeSet::new();
    let mut symbols = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let mut cursor = 0;
        while let Some(relative) = raw[cursor..].find("{%") {
            let start = cursor + relative + 2;
            let Some(end) = raw[start..].find("%}") else {
                break;
            };
            let body = raw[start..start + end].trim();
            let keyword = body.split_whitespace().next().unwrap_or("");
            let values = quoted_values(body);
            match keyword {
                "include" | "extends" | "embed" | "use" => {
                    if let Some(target) = values.first().and_then(|value| template_name(value)) {
                        relations.insert(("renders", target));
                    }
                }
                "import" | "from" => {
                    if let Some(target) = values.first().and_then(|value| template_name(value)) {
                        relations.insert(("imports", target));
                    }
                }
                "macro" => {
                    if let Some(label) = body.split_whitespace().nth(1) {
                        let label = label.split('(').next().unwrap_or(label);
                        symbols.push(file_symbol("function", label, offset + 1, Vec::new()));
                    }
                }
                "block" => {
                    if let Some(label) = body.split_whitespace().nth(1) {
                        symbols.push(file_symbol("component", label, offset + 1, Vec::new()));
                    }
                }
                _ => {}
            }
            cursor = start + end + 2;
        }
        let mut cursor = 0;
        while let Some(relative) = raw[cursor..].find("{{") {
            let start = cursor + relative + 2;
            let Some(end) = raw[start..].find("}}") else {
                break;
            };
            if let Some(target) = leading_identifier(raw[start..start + end].trim()) {
                relations.insert(("binds", target));
            }
            cursor = start + end + 2;
        }
    }
    symbols.insert(
        0,
        file_symbol(
            "component",
            name,
            1,
            relations
                .into_iter()
                .map(|(kind, target)| relation(kind, target))
                .collect(),
        ),
    );
    symbols
}

fn template_name(value: &str) -> Option<String> {
    let file = value.rsplit('/').next()?;
    let name = file
        .strip_suffix(".html.twig")
        .or_else(|| file.strip_suffix(".twig"))
        .unwrap_or(file);
    (!name.is_empty()).then(|| name.to_owned())
}

fn leading_identifier(value: &str) -> Option<String> {
    let end = value
        .find(|character: char| !(character == '_' || character.is_alphanumeric()))
        .unwrap_or(value.len());
    value
        .get(..end)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn xml_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let Some(mapper) = tag_attribute(source, "mapper", "namespace") else {
        return Vec::new();
    };
    let mut symbols = vec![file_symbol("module", mapper, 1, Vec::new())];
    for (offset, line) in source.lines().enumerate() {
        for tag in ["select", "insert", "update", "delete"] {
            if let Some(id) = tag_attribute(line, tag, "id") {
                let mut relations = vec![relation("binds", id.clone())];
                if let Some(reference) = tag_attribute(line, tag, "resultMap") {
                    relations.push(relation("references", reference));
                }
                symbols.push(file_symbol("query", id, offset + 1, relations));
            }
        }
        if let Some(reference) = tag_attribute(line, "include", "refid") {
            symbols.push(file_symbol(
                "query_fragment",
                reference.clone(),
                offset + 1,
                vec![relation("references", reference)],
            ));
        }
    }
    symbols
}

fn tag_attribute(source: &str, tag: &str, attribute: &str) -> Option<String> {
    let start = source.find(&format!("<{tag}"))?;
    let end = source[start..].find('>')? + start;
    let tag = &source[start..end];
    let marker = format!("{attribute}=");
    let value = tag.split_once(&marker)?.1.trim_start();
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let value = &value[quote.len_utf8()..];
    let end = value.find(quote)?;
    Some(value[..end].to_owned())
}

fn terraform_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        let Some((kind, label)) = terraform_block(line) else {
            index += 1;
            continue;
        };
        let start = index;
        let mut depth = brace_delta(line);
        index += 1;
        while index < lines.len() && depth > 0 {
            depth += brace_delta(lines[index]);
            index += 1;
        }
        let body = lines[start..index].join("\n");
        let mut relations = terraform_references(&body);
        if kind == "module" {
            for body_line in body.lines() {
                if body_line.trim_start().starts_with("source")
                    && let Some(source) = quoted_values(body_line).first()
                {
                    relations.insert(("imports", source.clone()));
                }
            }
        }
        symbols.push(file_symbol(
            kind,
            label,
            start + 1,
            relations
                .into_iter()
                .map(|(kind, target)| relation(kind, target))
                .collect(),
        ));
    }
    symbols
}

fn terraform_block(line: &str) -> Option<(&'static str, String)> {
    if !line.ends_with('{') {
        return None;
    }
    let keyword = line.split_whitespace().next()?;
    let values = quoted_values(line);
    match keyword {
        "resource" if values.len() >= 2 => {
            Some(("resource", format!("{}.{}", values[0], values[1])))
        }
        "data" if values.len() >= 2 => Some(("data", format!("{}.{}", values[0], values[1]))),
        "module" if !values.is_empty() => Some(("module", values[0].clone())),
        "variable" if !values.is_empty() => Some(("variable", values[0].clone())),
        "output" if !values.is_empty() => Some(("output", values[0].clone())),
        "provider" if !values.is_empty() => Some(("provider", values[0].clone())),
        "locals" => Some(("module", "locals".into())),
        "terraform" => Some(("module", "terraform".into())),
        _ => None,
    }
}

fn terraform_references(source: &str) -> BTreeSet<(&'static str, String)> {
    let mut relations = BTreeSet::new();
    for token in source.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '=' | '{' | '}' | '(' | ')' | '[' | ']' | ',' | '"'
            )
    }) {
        let token = token.trim_matches(|character: char| matches!(character, '\'' | ':' | ';'));
        let parts = token.split('.').collect::<Vec<_>>();
        let target = if parts.first() == Some(&"data") && parts.len() >= 3 {
            Some(format!("{}.{}", parts[1], parts[2]))
        } else if parts.first() == Some(&"module") && parts.len() >= 2 {
            Some(parts[1].to_owned())
        } else if parts.len() >= 2
            && !matches!(parts[0], "var" | "local" | "path" | "each" | "count")
        {
            Some(format!("{}.{}", parts[0], parts[1]))
        } else {
            None
        };
        if let Some(target) = target {
            relations.insert(("references", target));
        }
    }
    relations
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0, |depth, character| match character {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn nix_symbols(_path: &Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    let imports = source
        .lines()
        .flat_map(|line| {
            ["import ", "callPackage "]
                .into_iter()
                .filter_map(move |marker| line.split_once(marker).map(|(_, rest)| rest))
        })
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|target| target.trim_matches(['\'', '"', ';']).to_owned())
        .collect::<BTreeSet<_>>();
    for (offset, raw) in source.lines().enumerate() {
        for statement in raw.split(';') {
            let line = statement
                .trim()
                .strip_prefix("let ")
                .unwrap_or(statement.trim());
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty()
                || name.contains(char::is_whitespace)
                || matches!(name, "let" | "in" | "inherit")
            {
                continue;
            }
            let kind = if value.contains(':') {
                "function"
            } else {
                "constant"
            };
            symbols.push(file_symbol(
                kind,
                name,
                offset + 1,
                imports
                    .iter()
                    .cloned()
                    .map(|target| relation("imports", target))
                    .collect(),
            ));
        }
    }
    symbols
}
