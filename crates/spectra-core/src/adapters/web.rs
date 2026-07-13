use std::{collections::BTreeSet, path::Path};

use tree_sitter::{Language, Node as SyntaxNode};

use super::{EmbeddedRegion, FileSymbol, LanguageAdapter, Relation, Scope};

pub(crate) struct SvelteAdapter;
pub(crate) struct VueAdapter;
pub(crate) struct AstroAdapter;
pub(crate) struct LiquidAdapter;

pub(crate) static SVELTE: SvelteAdapter = SvelteAdapter;
pub(crate) static VUE: VueAdapter = VueAdapter;
pub(crate) static ASTRO: AstroAdapter = AstroAdapter;
pub(crate) static LIQUID: LiquidAdapter = LiquidAdapter;

#[derive(Clone, Copy)]
enum WebFlavor {
    Svelte,
    Vue,
    Astro,
    Liquid,
}

macro_rules! web_adapter {
    ($adapter:ty, $id:literal, $extensions:expr, $language:expr, $flavor:expr) => {
        impl LanguageAdapter for $adapter {
            fn id(&self) -> &'static str {
                $id
            }

            fn extensions(&self) -> &'static [&'static str] {
                $extensions
            }

            fn language(&self, _path: &Path) -> Option<Language> {
                Some($language.into())
            }

            fn classify(
                &self,
                _node: SyntaxNode<'_>,
                _source: &[u8],
                _scopes: &[Scope],
            ) -> Option<&'static str> {
                None
            }

            fn file_symbols(&self, path: &Path, source: &str) -> Vec<FileSymbol> {
                web_symbols(path, source, $flavor)
            }

            fn embedded_regions(&self, source: &str) -> Vec<EmbeddedRegion> {
                embedded_regions(source, $flavor)
            }
        }
    };
}

web_adapter!(
    SvelteAdapter,
    "svelte",
    &["svelte"],
    tree_sitter_svelte_ng::LANGUAGE,
    WebFlavor::Svelte
);
web_adapter!(
    VueAdapter,
    "vue",
    &["vue"],
    tree_sitter_vue_next::LANGUAGE,
    WebFlavor::Vue
);
web_adapter!(
    AstroAdapter,
    "astro",
    &["astro"],
    tree_sitter_astro_next::LANGUAGE,
    WebFlavor::Astro
);
web_adapter!(
    LiquidAdapter,
    "liquid",
    &["liquid"],
    tree_sitter_html::LANGUAGE,
    WebFlavor::Liquid
);

fn web_symbols(path: &Path, source: &str, flavor: WebFlavor) -> Vec<FileSymbol> {
    let component = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("component")
        .to_owned();
    let mut relations = BTreeSet::new();
    if matches!(flavor, WebFlavor::Liquid) {
        liquid_relations(source, &mut relations);
    } else {
        for target in component_tags(source) {
            relations.insert(("renders", target));
        }
        for target in event_bindings(source, flavor) {
            relations.insert(("binds", target));
        }
    }
    let end_line = source.lines().count().max(1) as u32;
    let mut symbols = vec![FileSymbol {
        kind: "component",
        label: component,
        start_line: 1,
        end_line,
        relations: relations
            .into_iter()
            .map(|(kind, target)| Relation { kind, target })
            .collect(),
    }];
    if let Some(route) = route_from_path(path, flavor) {
        symbols.push(FileSymbol {
            kind: "route",
            label: route,
            start_line: 1,
            end_line: 1,
            relations: Vec::new(),
        });
    }
    symbols
}

fn embedded_regions(source: &str, flavor: WebFlavor) -> Vec<EmbeddedRegion> {
    match flavor {
        WebFlavor::Svelte | WebFlavor::Vue => script_regions(source),
        WebFlavor::Astro => frontmatter_region(source).into_iter().collect(),
        WebFlavor::Liquid => Vec::new(),
    }
}

fn script_regions(source: &str) -> Vec<EmbeddedRegion> {
    let mut regions = Vec::new();
    let mut cursor = 0;
    while let Some(relative_open) = source[cursor..].find("<script") {
        let open = cursor + relative_open;
        let Some(relative_open_end) = source[open..].find('>') else {
            break;
        };
        let open_end = open + relative_open_end;
        let Some(relative_close) = source[open_end + 1..].find("</script>") else {
            break;
        };
        let close = open_end + 1 + relative_close;
        let attributes = &source[open..=open_end];
        let language = if attributes.contains("lang=\"ts\"")
            || attributes.contains("lang='ts'")
            || attributes.contains("lang=\"typescript\"")
            || attributes.contains("lang='typescript'")
        {
            "typescript"
        } else {
            "javascript"
        };
        regions.push(EmbeddedRegion {
            language,
            start_byte: open_end + 1,
            end_byte: close,
        });
        cursor = close + "</script>".len();
    }
    regions
}

fn frontmatter_region(source: &str) -> Option<EmbeddedRegion> {
    let mut offset = 0;
    let mut delimiter = None;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let line_start = offset;
        offset += line.len();
        if line.trim() != "---" {
            continue;
        }
        if index == 0 {
            delimiter = Some(offset);
        } else if let Some(start_byte) = delimiter {
            return Some(EmbeddedRegion {
                language: "typescript",
                start_byte,
                end_byte: line_start,
            });
        }
    }
    None
}

fn component_tags(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut names = BTreeSet::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<' || cursor + 1 >= bytes.len() {
            cursor += 1;
            continue;
        }
        let start = cursor + 1;
        if matches!(bytes[start], b'/' | b'!' | b'?' | b'#') || !bytes[start].is_ascii_uppercase() {
            cursor += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'.' | b':'))
        {
            end += 1;
        }
        if let Some(name) = source.get(start..end).filter(|name| !name.is_empty()) {
            names.insert(name.rsplit(['.', ':']).next().unwrap_or(name).to_owned());
        }
        cursor = end;
    }
    names.into_iter().collect()
}

fn event_bindings(source: &str, flavor: WebFlavor) -> Vec<String> {
    let markers: &[&str] = match flavor {
        WebFlavor::Svelte => &["on:"],
        WebFlavor::Vue => &["@", "v-on:"],
        WebFlavor::Astro => &["onClick", "onChange", "onInput", "onSubmit"],
        WebFlavor::Liquid => &[],
    };
    let mut names = BTreeSet::new();
    for marker in markers {
        let mut cursor = 0;
        while let Some(relative) = source[cursor..].find(marker) {
            let start = cursor + relative + marker.len();
            let rest = &source[start..];
            let Some(equal) = rest.find('=') else {
                break;
            };
            if rest[..equal].contains(['>', '<', '\n']) {
                cursor = start;
                continue;
            }
            if let Some(name) = leading_value_identifier(&rest[equal + 1..]) {
                names.insert(name);
            }
            cursor = start + equal + 1;
        }
    }
    names.into_iter().collect()
}

fn leading_value_identifier(value: &str) -> Option<String> {
    let value = value.trim_start();
    let value = value
        .strip_prefix(['"', '\'', '{'])
        .unwrap_or(value)
        .trim_start();
    let end = value
        .find(|character: char| {
            !(character == '_' || character == '$' || character.is_alphanumeric())
        })
        .unwrap_or(value.len());
    value
        .get(..end)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn liquid_relations(source: &str, relations: &mut BTreeSet<(&'static str, String)>) {
    let mut cursor = 0;
    loop {
        let tag = source[cursor..].find("{%");
        let output_tag = source[cursor..].find("{{");
        let Some(relative) = (match (tag, output_tag) {
            (Some(tag), Some(output)) => Some(tag.min(output)),
            (Some(tag), None) => Some(tag),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        }) else {
            break;
        };
        let start = cursor + relative;
        let output = source[start..].starts_with("{{");
        let close = if output { "}}" } else { "%}" };
        let Some(relative_end) = source[start + 2..].find(close) else {
            break;
        };
        let end = start + 2 + relative_end;
        let body = source[start + 2..end].trim();
        if output {
            if let Some(name) = body
                .split(|character: char| !(character == '_' || character.is_alphanumeric()))
                .find(|part| !part.is_empty())
            {
                relations.insert(("binds", name.to_owned()));
            }
        } else if let Some(rest) = body
            .strip_prefix("render ")
            .or_else(|| body.strip_prefix("include "))
        {
            let target = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(['\'', '"'])
                .rsplit('/')
                .next()
                .unwrap_or("")
                .trim_end_matches(".liquid");
            if !target.is_empty() {
                relations.insert(("renders", target.to_owned()));
            }
        }
        cursor = end + close.len();
    }
}

fn route_from_path(path: &Path, flavor: WebFlavor) -> Option<String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let marker = match flavor {
        WebFlavor::Svelte => "routes/",
        WebFlavor::Vue | WebFlavor::Astro => "pages/",
        WebFlavor::Liquid => return None,
    };
    let relative = normalized.split_once(marker)?.1;
    let mut segments: Vec<_> = relative.split('/').map(str::to_owned).collect();
    let file = segments.pop()?;
    let stem = file.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&file);
    if matches!(flavor, WebFlavor::Svelte) {
        if stem != "+page" {
            return None;
        }
    } else if stem != "index" {
        segments.push(stem.to_owned());
    }
    let segments: Vec<_> = segments
        .into_iter()
        .filter(|segment| !(segment.starts_with('(') && segment.ends_with(')')))
        .map(|segment| dynamic_route_segment(&segment))
        .collect();
    if segments.is_empty() {
        Some("/".into())
    } else {
        Some(format!("/{}", segments.join("/")))
    }
}

fn dynamic_route_segment(segment: &str) -> String {
    if let Some(parameter) = segment
        .strip_prefix("[...")
        .and_then(|value| value.strip_suffix(']'))
    {
        format!("*{parameter}")
    } else if let Some(parameter) = segment
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        format!(":{parameter}")
    } else {
        segment.to_owned()
    }
}
