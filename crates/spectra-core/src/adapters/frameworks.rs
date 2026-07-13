use super::{FileSymbol, file_symbol, quoted_values, relation};

pub(crate) fn javascript_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for receiver in ["app", "router"] {
        for method in [
            "get", "post", "put", "patch", "delete", "options", "head", "all", "use",
        ] {
            let marker = format!("{receiver}.{method}(");
            for call in balanced_calls(source, &marker) {
                let args = split_args(call.body);
                let Some(path) = args.first().and_then(|arg| first_quoted(arg)) else {
                    continue;
                };
                if method == "use" && !path.starts_with('/') {
                    continue;
                }
                let relations = args
                    .last()
                    .and_then(|handler| handler_name(handler))
                    .map(|handler| vec![relation("routes_to", handler)])
                    .unwrap_or_default();
                symbols.push(file_symbol(
                    "route",
                    format!("{} {path}", method.to_ascii_uppercase()),
                    call.line,
                    relations,
                ));
            }
        }
    }
    symbols.extend(nestjs_routes(source));
    symbols
}

fn nestjs_routes(source: &str) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let controller = lines.iter().find_map(|line| {
        let line = line.trim();
        line.starts_with("@Controller(")
            .then(|| first_quoted(line))
            .flatten()
    });
    let controller_name = lines.iter().find_map(|line| class_name(line));
    let mut symbols = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let Some((method, path)) = nest_decorator(trimmed) else {
            continue;
        };
        let Some(handler) = lines
            .iter()
            .skip(index + 1)
            .find_map(|line| method_name(line))
        else {
            continue;
        };
        let target = controller_name
            .as_deref()
            .map(|controller| format!("{controller}::{handler}"))
            .unwrap_or(handler);
        let path = join_route(controller.as_deref().unwrap_or(""), &path);
        symbols.push(file_symbol(
            "route",
            format!("{method} {path}"),
            index + 1,
            vec![relation("routes_to", target)],
        ));
    }
    symbols
}

pub(crate) fn python_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for marker in ["path(", "re_path(", "url("] {
        for call in balanced_calls(source, marker) {
            let args = split_args(call.body);
            let (Some(path), Some(handler)) = (
                args.first().and_then(|arg| first_quoted(arg)),
                args.get(1).and_then(|arg| handler_name(arg)),
            ) else {
                continue;
            };
            symbols.push(file_symbol(
                "route",
                path,
                call.line,
                vec![relation("routes_to", handler)],
            ));
        }
    }
    for call in balanced_calls(source, ".register(") {
        let args = split_args(call.body);
        let (Some(prefix), Some(handler)) = (
            args.first().and_then(|arg| first_quoted(arg)),
            args.get(1).and_then(|arg| handler_name(arg)),
        ) else {
            continue;
        };
        symbols.push(file_symbol(
            "route",
            format!("VIEWSET /{}", prefix.trim_matches('/')),
            call.line,
            vec![relation("routes_to", handler)],
        ));
    }
    symbols.extend(python_decorator_routes(source));
    symbols
}

fn python_decorator_routes(source: &str) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with('@') {
            continue;
        }
        let Some(path) = first_quoted(trimmed) else {
            continue;
        };
        let method = ["get", "post", "put", "patch", "delete", "options", "head"]
            .into_iter()
            .find(|method| trimmed.contains(&format!(".{method}(")))
            .map(str::to_ascii_uppercase)
            .or_else(|| trimmed.contains(".route(").then(|| "ANY".to_owned()));
        let Some(method) = method else {
            continue;
        };
        let Some(handler) = lines
            .iter()
            .skip(index + 1)
            .find_map(|line| python_function_name(line))
        else {
            continue;
        };
        symbols.push(file_symbol(
            "route",
            format!("{method} {}", normalized_route(&path)),
            index + 1,
            vec![relation("routes_to", handler)],
        ));
    }
    symbols
}

pub(crate) fn ruby_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let Some(method) = ["get", "post", "put", "patch", "delete", "match"]
            .into_iter()
            .find(|method| trimmed.starts_with(&format!("{method} ")))
        else {
            if let Some(resource) = rails_resource(trimmed) {
                symbols.extend(rails_resource_routes(&resource, index + 1));
            }
            continue;
        };
        let quoted = quoted_values(trimmed);
        let Some(path) = quoted.first() else {
            continue;
        };
        let Some((controller, action)) = quoted.iter().find_map(|value| value.split_once('#'))
        else {
            continue;
        };
        symbols.push(file_symbol(
            "route",
            format!("{} {path}", method.to_ascii_uppercase()),
            index + 1,
            vec![relation(
                "routes_to",
                format!("{}Controller::{action}", pascal_case(controller)),
            )],
        ));
    }
    symbols
}

pub(crate) fn php_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for method in ["get", "post", "put", "patch", "delete", "options", "any"] {
        let marker = format!("Route::{method}(");
        for call in balanced_calls(source, &marker) {
            let args = split_args(call.body);
            let Some(path) = args.first().and_then(|arg| first_quoted(arg)) else {
                continue;
            };
            let relations = args
                .get(1)
                .and_then(|handler| php_handler_name(handler))
                .map(|handler| vec![relation("routes_to", handler)])
                .unwrap_or_default();
            symbols.push(file_symbol(
                "route",
                format!("{} {path}", method.to_ascii_uppercase()),
                call.line,
                relations,
            ));
        }
    }
    for call in balanced_calls(source, "Route::resource(") {
        let args = split_args(call.body);
        let (Some(path), Some(controller)) = (
            args.first().and_then(|arg| first_quoted(arg)),
            args.get(1).and_then(|arg| class_reference(arg)),
        ) else {
            continue;
        };
        for (method, suffix, action) in resource_actions() {
            symbols.push(file_symbol(
                "route",
                format!("{method} {}", resource_path(&path, suffix)),
                call.line,
                vec![relation("routes_to", format!("{controller}::{action}"))],
            ));
        }
    }
    symbols
}

pub(crate) fn java_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = annotation_routes(
        source,
        "RequestMapping",
        &[
            ("GetMapping", "GET"),
            ("PostMapping", "POST"),
            ("PutMapping", "PUT"),
            ("PatchMapping", "PATCH"),
            ("DeleteMapping", "DELETE"),
        ],
        java_method_name,
    );
    let lines = source.lines().collect::<Vec<_>>();
    let class = lines.iter().find_map(|line| class_name(line));
    let class_index = lines
        .iter()
        .position(|line| class_name(line).is_some())
        .unwrap_or(0);
    let prefix = lines[..class_index]
        .iter()
        .rev()
        .find_map(|line| annotation_path(line, "RequestMapping"))
        .unwrap_or_default();
    for (index, line) in lines.iter().enumerate().skip(class_index + 1) {
        let trimmed = line.trim();
        if !trimmed.starts_with("@RequestMapping") {
            continue;
        }
        let method = ["GET", "POST", "PUT", "PATCH", "DELETE"]
            .into_iter()
            .find(|method| trimmed.contains(&format!("RequestMethod.{method}")))
            .unwrap_or("ANY");
        let path = annotation_path(trimmed, "RequestMapping").unwrap_or_default();
        let Some(handler) = lines
            .iter()
            .skip(index + 1)
            .find_map(|line| java_method_name(line))
        else {
            continue;
        };
        let target = class
            .as_deref()
            .map(|class| format!("{class}::{handler}"))
            .unwrap_or(handler);
        symbols.push(file_symbol(
            "route",
            format!("{method} {}", join_route(&prefix, &path)),
            index + 1,
            vec![relation("routes_to", target)],
        ));
    }
    symbols
}

pub(crate) fn csharp_routes(source: &str) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let class = lines.iter().find_map(|line| class_name(line));
    let controller = class
        .as_deref()
        .map(|name| name.strip_suffix("Controller").unwrap_or(name));
    let prefix = lines
        .iter()
        .take_while(|line| !line.contains(" class ") && !line.trim_start().starts_with("class "))
        .filter_map(|line| attribute_path(line, "Route"))
        .last()
        .unwrap_or_default()
        .replace("[controller]", controller.unwrap_or(""));
    let mut symbols = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        for (attribute, method) in [
            ("HttpGet", "GET"),
            ("HttpPost", "POST"),
            ("HttpPut", "PUT"),
            ("HttpPatch", "PATCH"),
            ("HttpDelete", "DELETE"),
        ] {
            if !trimmed.starts_with(&format!("[{attribute}")) {
                continue;
            }
            let path = attribute_path(trimmed, attribute).unwrap_or_default();
            let Some(handler) = lines
                .iter()
                .skip(index + 1)
                .find_map(|line| csharp_method_name(line))
            else {
                continue;
            };
            let target = class
                .as_deref()
                .map(|class| format!("{class}::{handler}"))
                .unwrap_or(handler);
            symbols.push(file_symbol(
                "route",
                format!("{method} {}", join_route(&prefix, &path)),
                index + 1,
                vec![relation("routes_to", target)],
            ));
        }
    }
    for (marker, method) in [
        (".MapGet(", "GET"),
        (".MapPost(", "POST"),
        (".MapPut(", "PUT"),
        (".MapPatch(", "PATCH"),
        (".MapDelete(", "DELETE"),
    ] {
        for call in balanced_calls(source, marker) {
            let args = split_args(call.body);
            let (Some(path), Some(handler)) = (
                args.first().and_then(|arg| first_quoted(arg)),
                args.get(1).and_then(|arg| handler_name(arg)),
            ) else {
                continue;
            };
            symbols.push(file_symbol(
                "route",
                format!("{method} {path}"),
                call.line,
                vec![relation("routes_to", handler)],
            ));
        }
    }
    symbols
}

pub(crate) fn go_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for (marker, method) in [
        (".GET(", "GET"),
        (".Get(", "GET"),
        (".POST(", "POST"),
        (".Post(", "POST"),
        (".PUT(", "PUT"),
        (".Put(", "PUT"),
        (".PATCH(", "PATCH"),
        (".Patch(", "PATCH"),
        (".DELETE(", "DELETE"),
        (".Delete(", "DELETE"),
        (".Handle(", "ANY"),
        (".HandleFunc(", "ANY"),
    ] {
        for call in balanced_calls(source, marker) {
            let args = split_args(call.body);
            let (Some(path), Some(handler)) = (
                args.first().and_then(|arg| first_quoted(arg)),
                args.last().and_then(|arg| handler_name(arg)),
            ) else {
                continue;
            };
            symbols.push(file_symbol(
                "route",
                format!("{method} {path}"),
                call.line,
                vec![relation("routes_to", handler)],
            ));
        }
    }
    for (index, line) in source.lines().enumerate() {
        if !line.contains("g.Meta") {
            continue;
        }
        let Some(path) = tagged_value(line, "path") else {
            continue;
        };
        let method = tagged_value(line, "method").unwrap_or_else(|| "ANY".to_owned());
        symbols.push(file_symbol(
            "route",
            format!("{} {path}", method.to_ascii_uppercase()),
            index + 1,
            Vec::new(),
        ));
    }
    symbols
}

pub(crate) fn rust_routes(source: &str) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            if !trimmed.starts_with(&format!("#[{method}(")) {
                continue;
            }
            let (Some(path), Some(handler)) = (
                first_quoted(trimmed),
                lines
                    .iter()
                    .skip(index + 1)
                    .find_map(|line| rust_function_name(line)),
            ) else {
                continue;
            };
            symbols.push(file_symbol(
                "route",
                format!("{} {path}", method.to_ascii_uppercase()),
                index + 1,
                vec![relation("routes_to", handler)],
            ));
        }
    }
    for call in balanced_calls(source, ".route(") {
        let args = split_args(call.body);
        let (Some(path), Some(routes)) =
            (args.first().and_then(|arg| first_quoted(arg)), args.get(1))
        else {
            continue;
        };
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            for handler_call in balanced_calls(routes, &format!("{method}(")) {
                let Some(handler) = handler_name(handler_call.body) else {
                    continue;
                };
                symbols.push(file_symbol(
                    "route",
                    format!("{} {path}", method.to_ascii_uppercase()),
                    call.line,
                    vec![relation("routes_to", handler)],
                ));
            }
            let actix = format!("web::{method}().to(");
            for handler_call in balanced_calls(routes, &actix) {
                let Some(handler) = handler_name(handler_call.body) else {
                    continue;
                };
                symbols.push(file_symbol(
                    "route",
                    format!("{} {path}", method.to_ascii_uppercase()),
                    call.line,
                    vec![relation("routes_to", handler)],
                ));
            }
        }
    }
    symbols
}

pub(crate) fn swift_routes(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
        for call in balanced_calls(source, &format!(".{method}(")) {
            let args = split_args(call.body);
            let Some(use_arg) = args.iter().find(|arg| arg.trim_start().starts_with("use:")) else {
                continue;
            };
            let path = args
                .iter()
                .take_while(|arg| !arg.trim_start().starts_with("use:"))
                .filter_map(|arg| first_quoted(arg))
                .collect::<Vec<_>>()
                .join("/");
            let Some(handler) = handler_name(use_arg.trim_start_matches("use:")) else {
                continue;
            };
            symbols.push(file_symbol(
                "route",
                format!(
                    "{} {}",
                    method.to_ascii_uppercase(),
                    normalized_route(&path)
                ),
                call.line,
                vec![relation("routes_to", handler)],
            ));
        }
    }
    symbols
}

pub(crate) fn play_routes(path: &std::path::Path, source: &str) -> Vec<FileSymbol> {
    if path.file_name().and_then(|name| name.to_str()) != Some("routes")
        && path.extension().and_then(|extension| extension.to_str()) != Some("routes")
    {
        return Vec::new();
    }
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("->") {
                return None;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next()?;
            let path = parts.next()?;
            let action = parts.next()?.split('(').next()?;
            let target = action.replace('.', "::");
            Some(file_symbol(
                "route",
                format!("{} {path}", method.to_ascii_uppercase()),
                index + 1,
                vec![relation("routes_to", target)],
            ))
        })
        .collect()
}

pub(crate) fn react_routes(path: &std::path::Path, source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find("<Route") {
        let start = cursor + relative;
        let end = (start + 400).min(source.len());
        let window = &source[start..end];
        if let Some(route) = jsx_attribute(window, "path") {
            let component = jsx_component_attribute(window);
            symbols.push(file_symbol(
                "route",
                route,
                line_of(source, start),
                component
                    .map(|component| vec![relation("routes_to", component)])
                    .unwrap_or_default(),
            ));
        }
        cursor = start + "<Route".len();
    }

    if [
        "createBrowserRouter",
        "createHashRouter",
        "createMemoryRouter",
        "createRoutesFromElements",
    ]
    .iter()
    .any(|marker| source.contains(marker))
    {
        let mut cursor = 0;
        while let Some(relative) = source[cursor..].find("path:") {
            let start = cursor + relative;
            let end = (start + 300).min(source.len());
            let window = &source[start..end];
            if let (Some(route), Some(component)) =
                (first_quoted(window), object_component_attribute(window))
            {
                symbols.push(file_symbol(
                    "route",
                    normalized_route(&route),
                    line_of(source, start),
                    vec![relation("routes_to", component)],
                ));
            }
            cursor = start + "path:".len();
        }
    }

    if source.contains("export default")
        && let Some(route) = next_route_from_path(path)
    {
        let offset = source.find("export default").unwrap_or(0);
        let relations = default_export_name(&source[offset..])
            .map(|component| vec![relation("routes_to", component)])
            .unwrap_or_default();
        symbols.push(file_symbol(
            "route",
            route,
            line_of(source, offset),
            relations,
        ));
    }
    symbols
}

pub(crate) fn fabric_typescript_components(source: &str) -> Vec<FileSymbol> {
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            line.contains("codegenNativeComponent")
                .then(|| first_quoted(line))
                .flatten()
                .map(|name| file_symbol("component", name, index + 1, Vec::new()))
        })
        .collect()
}

pub(crate) fn objective_c_bridge_symbols(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    if source.contains("RCT_EXPORT_MODULE") {
        for call in balanced_calls(source, "RCT_EXPORT_METHOD(") {
            if let Some(name) = selector_keyword(call.body) {
                symbols.push(file_symbol("method", name, call.line, Vec::new()));
            }
        }
        for call in balanced_calls(source, "RCT_REMAP_METHOD(") {
            if let Some(name) = split_args(call.body)
                .first()
                .and_then(|name| handler_name(name))
            {
                symbols.push(file_symbol("method", name, call.line, Vec::new()));
            }
        }
    }
    if let Some(class) = source.lines().find_map(|line| {
        line.trim()
            .strip_prefix("@implementation ")
            .and_then(|rest| rest.split_whitespace().next())
            .filter(|name| name.ends_with("Manager") || name.ends_with("ViewManager"))
    }) {
        symbols.push(file_symbol(
            "component",
            native_component_name(class),
            source
                .lines()
                .position(|line| line.contains("@implementation"))
                .unwrap_or(0)
                + 1,
            Vec::new(),
        ));
    }
    symbols
}

pub(crate) fn swift_client_symbols(source: &str) -> Vec<FileSymbol> {
    let mut symbols = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let words = line
            .split(|character: char| !is_identifier_character(character))
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        if words.first() == Some(&"struct")
            && words.get(2) == Some(&"View")
            && let Some(name) = words.get(1)
        {
            symbols.push(file_symbol("component", *name, index + 1, Vec::new()));
        }
    }
    symbols.extend(expo_module_symbols(source));
    symbols
}

pub(crate) fn jvm_client_symbols(source: &str) -> Vec<FileSymbol> {
    let mut symbols = expo_module_symbols(source);
    if source.contains("ViewManager")
        && let Some((line, class)) = source
            .lines()
            .enumerate()
            .find_map(|(index, line)| class_name(line).map(|class| (index + 1, class)))
        && (class.ends_with("Manager") || class.ends_with("ViewManager"))
    {
        symbols.push(file_symbol(
            "component",
            native_component_name(&class),
            line,
            Vec::new(),
        ));
    }
    symbols
}

fn expo_module_symbols(source: &str) -> Vec<FileSymbol> {
    if !(source.contains(": Module") || source.contains("extends Module")) {
        return Vec::new();
    }
    let module = balanced_calls(source, "Name(")
        .into_iter()
        .find_map(|call| first_quoted(call.body))
        .or_else(|| source.lines().find_map(class_name))
        .unwrap_or_else(|| "ExpoModule".to_owned());
    let mut symbols = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, line) in source.lines().enumerate() {
        for kind in ["Function", "AsyncFunction", "Property", "Constants"] {
            let Some(offset) = line.find(kind) else {
                continue;
            };
            if let Some(name) = first_quoted(&line[offset + kind.len()..])
                && seen.insert(name.clone())
            {
                symbols.push(file_symbol(
                    "method",
                    name.clone(),
                    index + 1,
                    vec![relation("exports", format!("{module}::{name}"))],
                ));
            }
        }
    }
    symbols
}

struct BalancedCall<'a> {
    body: &'a str,
    line: usize,
}

fn balanced_calls<'a>(source: &'a str, marker: &str) -> Vec<BalancedCall<'a>> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    let head = marker.strip_suffix('(').unwrap_or(marker);
    while let Some(relative) = source[cursor..].find(head) {
        let start = cursor + relative;
        if head.chars().next().is_some_and(is_identifier_character)
            && source[..start]
                .chars()
                .next_back()
                .is_some_and(is_identifier_character)
        {
            cursor = start + head.len();
            continue;
        }
        let mut open = start + head.len();
        while source[open..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            open += source[open..].chars().next().unwrap().len_utf8();
        }
        if !source[open..].starts_with('(') {
            cursor = open.min(source.len());
            continue;
        }
        let body_start = open + 1;
        let mut depth = 1usize;
        let mut quote = None;
        let mut escaped = false;
        let mut end = body_start;
        for (offset, character) in source[body_start..].char_indices() {
            end = body_start + offset;
            if let Some(open) = quote {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == open {
                    quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' | '`' => quote = Some(character),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        calls.push(BalancedCall {
                            body: &source[body_start..end],
                            line: source[..start]
                                .bytes()
                                .filter(|byte| *byte == b'\n')
                                .count()
                                + 1,
                        });
                        end += character.len_utf8();
                        break;
                    }
                }
                _ => {}
            }
        }
        cursor = end.max(body_start).min(source.len());
        if cursor == start {
            cursor += head.len();
        }
    }
    calls
}

fn split_args(body: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(body[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    args.push(body[start..].trim());
    args
}

fn first_quoted(value: &str) -> Option<String> {
    quoted_values(value).into_iter().next()
}

fn handler_name(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches("async ");
    if value.contains("=>") || value.starts_with("function") {
        return None;
    }
    let value = value
        .trim_matches(|character: char| matches!(character, '&' | '*' | '(' | ')' | '[' | ']'))
        .split_whitespace()
        .next()?;
    value
        .rsplit(['.', ':'])
        .find(|part| is_identifier(part))
        .map(str::to_owned)
}

fn php_handler_name(value: &str) -> Option<String> {
    if let Some(handler) = first_quoted(value)
        && let Some((controller, method)) = handler.split_once('@')
    {
        return Some(format!("{}::{method}", terminal_class(controller)));
    }
    let controller = class_reference(value)?;
    let method = quoted_values(value).last()?.to_owned();
    Some(format!("{controller}::{method}"))
}

fn class_reference(value: &str) -> Option<String> {
    let prefix = value
        .split("::class")
        .next()?
        .trim()
        .trim_matches(|character: char| character != '\\' && !is_identifier_character(character));
    let class = terminal_class(prefix);
    is_identifier(&class).then_some(class)
}

fn annotation_routes(
    source: &str,
    prefix_annotation: &str,
    mappings: &[(&str, &str)],
    method_parser: fn(&str) -> Option<String>,
) -> Vec<FileSymbol> {
    let lines = source.lines().collect::<Vec<_>>();
    let class = lines.iter().find_map(|line| class_name(line));
    let class_index = lines
        .iter()
        .position(|line| class_name(line).is_some())
        .unwrap_or(0);
    let prefix = lines[..class_index]
        .iter()
        .rev()
        .find_map(|line| annotation_path(line, prefix_annotation))
        .unwrap_or_default();
    let mut symbols = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        for (annotation, method) in mappings {
            if !trimmed.starts_with(&format!("@{annotation}")) {
                continue;
            }
            let path = annotation_path(trimmed, annotation).unwrap_or_default();
            let Some(handler) = lines
                .iter()
                .skip(index + 1)
                .find_map(|line| method_parser(line))
            else {
                continue;
            };
            let target = class
                .as_deref()
                .map(|class| format!("{class}::{handler}"))
                .unwrap_or(handler);
            symbols.push(file_symbol(
                "route",
                format!("{method} {}", join_route(&prefix, &path)),
                index + 1,
                vec![relation("routes_to", target)],
            ));
        }
    }
    symbols
}

fn annotation_path(line: &str, annotation: &str) -> Option<String> {
    line.find(&format!("@{annotation}"))?;
    first_quoted(line).or_else(|| Some(String::new()))
}

fn attribute_path(line: &str, attribute: &str) -> Option<String> {
    line.find(&format!("[{attribute}"))?;
    first_quoted(line).or_else(|| Some(String::new()))
}

fn java_method_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with('@') || !trimmed.contains('(') {
        return None;
    }
    let name = trimmed.split('(').next()?.split_whitespace().last()?;
    is_identifier(name).then(|| name.to_owned())
}

fn csharp_method_name(line: &str) -> Option<String> {
    java_method_name(line)
}

fn rust_function_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("pub async fn ")
        .or_else(|| trimmed.strip_prefix("pub fn "))
        .or_else(|| trimmed.strip_prefix("async fn "))
        .or_else(|| trimmed.strip_prefix("fn "))?;
    let name = rest.split('(').next()?.trim();
    is_identifier(name).then(|| name.to_owned())
}

fn tagged_value(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}:\"");
    let start = line.find(&marker)? + marker.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}

fn jsx_attribute(window: &str, attribute: &str) -> Option<String> {
    let marker = format!("{attribute}=");
    let compact = window.replace([' ', '\n', '\r', '\t'], "");
    let start = compact.find(&marker)? + marker.len();
    first_quoted(&compact[start..])
}

fn jsx_component_attribute(window: &str) -> Option<String> {
    for marker in ["component={", "element={<"] {
        let compact = window.replace([' ', '\n', '\r', '\t'], "");
        if let Some(start) = compact.find(marker) {
            return leading_name(&compact[start + marker.len()..]);
        }
    }
    None
}

fn object_component_attribute(window: &str) -> Option<String> {
    let compact = window.replace([' ', '\n', '\r', '\t'], "");
    for marker in ["element:<", "Component:"] {
        if let Some(start) = compact.find(marker) {
            return leading_name(&compact[start + marker.len()..]);
        }
    }
    None
}

fn leading_name(value: &str) -> Option<String> {
    let name = value
        .chars()
        .take_while(|character| is_identifier_character(*character))
        .collect::<String>();
    name.chars()
        .next()
        .is_some_and(char::is_uppercase)
        .then_some(name)
}

fn default_export_name(source: &str) -> Option<String> {
    let rest = source.strip_prefix("export default")?.trim_start();
    let rest = rest
        .strip_prefix("async ")
        .unwrap_or(rest)
        .strip_prefix("function ")
        .unwrap_or(rest);
    leading_name(rest)
}

fn next_route_from_path(path: &std::path::Path) -> Option<String> {
    let parts = path
        .iter()
        .filter_map(|part| part.to_str())
        .collect::<Vec<_>>();
    let root = parts
        .iter()
        .position(|part| matches!(*part, "pages" | "app"))?;
    let mut route = Vec::new();
    for part in &parts[root + 1..] {
        let stem = part.split('.').next().unwrap_or(part);
        if matches!(stem, "index" | "page" | "layout")
            || stem.starts_with('(') && stem.ends_with(')')
        {
            continue;
        }
        let segment = if stem.starts_with("[...") && stem.ends_with(']') {
            format!("*{}", &stem[4..stem.len() - 1])
        } else if stem.starts_with('[') && stem.ends_with(']') {
            format!(":{}", &stem[1..stem.len() - 1])
        } else {
            stem.to_owned()
        };
        route.push(segment);
    }
    Some(normalized_route(&route.join("/")))
}

fn selector_keyword(value: &str) -> Option<String> {
    let name = value
        .trim()
        .chars()
        .take_while(|character| is_identifier_character(*character))
        .collect::<String>();
    is_identifier(&name).then_some(name)
}

fn native_component_name(class: &str) -> String {
    let class = class.strip_prefix("RCT").unwrap_or(class);
    class
        .strip_suffix("ViewManager")
        .or_else(|| class.strip_suffix("Manager"))
        .or_else(|| class.strip_suffix("View"))
        .unwrap_or(class)
        .to_owned()
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn terminal_class(value: &str) -> String {
    value
        .rsplit(['\\', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(value)
        .to_owned()
}

fn nest_decorator(line: &str) -> Option<(String, String)> {
    for method in [
        "Get", "Post", "Put", "Patch", "Delete", "Options", "Head", "All",
    ] {
        let marker = format!("@{method}(");
        if line.starts_with(&marker) {
            return Some((
                method.to_ascii_uppercase(),
                first_quoted(line).unwrap_or_default(),
            ));
        }
    }
    None
}

fn class_name(line: &str) -> Option<String> {
    let words = line
        .split(|character: char| !is_identifier_character(character))
        .collect::<Vec<_>>();
    words
        .windows(2)
        .find_map(|words| (words[0] == "class").then(|| words[1].to_owned()))
}

fn method_name(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_start_matches("async ");
    let name = trimmed.split('(').next()?.split_whitespace().last()?;
    is_identifier(name).then(|| name.to_owned())
}

fn python_function_name(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_start_matches("async ");
    let rest = trimmed.strip_prefix("def ")?;
    let name = rest.split('(').next()?.trim();
    is_identifier(name).then(|| name.to_owned())
}

fn rails_resource(line: &str) -> Option<String> {
    let rest = line.strip_prefix("resources ")?.trim();
    let name = rest
        .trim_start_matches(':')
        .split([',', ' '])
        .next()?
        .trim_matches(['\'', '"']);
    is_identifier(name).then(|| name.to_owned())
}

fn rails_resource_routes(resource: &str, line: usize) -> Vec<FileSymbol> {
    let controller = format!("{}Controller", pascal_case(resource));
    resource_actions()
        .into_iter()
        .map(|(method, suffix, action)| {
            file_symbol(
                "route",
                format!("{method} {}", resource_path(resource, suffix)),
                line,
                vec![relation("routes_to", format!("{controller}::{action}"))],
            )
        })
        .collect()
}

fn resource_actions() -> [(&'static str, &'static str, &'static str); 7] {
    [
        ("GET", "", "index"),
        ("GET", "/new", "new"),
        ("POST", "", "create"),
        ("GET", "/:id", "show"),
        ("GET", "/:id/edit", "edit"),
        ("PUT", "/:id", "update"),
        ("DELETE", "/:id", "destroy"),
    ]
}

fn resource_path(base: &str, suffix: &str) -> String {
    format!("/{}{}", base.trim_matches('/'), suffix)
}

fn join_route(prefix: &str, path: &str) -> String {
    let joined = format!("{}/{}", prefix.trim_matches('/'), path.trim_matches('/'));
    normalized_route(&joined)
}

fn normalized_route(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        "/".to_owned()
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

fn pascal_case(value: &str) -> String {
    value
        .split(['_', '-', '/'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().chain(characters).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().all(is_identifier_character)
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character == '$' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_route(symbols: &[FileSymbol], label: &str, target: &str) -> bool {
        symbols.iter().any(|symbol| {
            symbol.kind == "route"
                && symbol.label == label
                && symbol
                    .relations
                    .iter()
                    .any(|edge| edge.kind == "routes_to" && edge.target == target)
        })
    }

    #[test]
    fn extracts_express_and_nestjs_routes() {
        let express = javascript_routes(
            "router.post(\n  '/users',\n  authenticate,\n  createUser\n);\nfunction createUser() {}",
        );
        assert!(has_route(&express, "POST /users", "createUser"));

        let nest = javascript_routes(
            "@Controller('admin/users')\nexport class UsersController {\n  @Get(':id')\n  show() {}\n}",
        );
        assert!(has_route(
            &nest,
            "GET /admin/users/:id",
            "UsersController::show"
        ));
    }

    #[test]
    fn extracts_django_flask_and_fastapi_routes() {
        let symbols = python_routes(
            "urlpatterns = [path('users/', views.users)]\n\n@app.route('/health')\ndef health(): pass\n\n@router.post('/users')\nasync def create_user(): pass\n",
        );
        assert!(has_route(&symbols, "users/", "users"));
        assert!(has_route(&symbols, "ANY /health", "health"));
        assert!(has_route(&symbols, "POST /users", "create_user"));
    }

    #[test]
    fn extracts_rails_and_laravel_routes() {
        let rails = ruby_routes("get '/users/:id', to: 'users#show'\nresources :accounts\n");
        assert!(has_route(&rails, "GET /users/:id", "UsersController::show"));
        assert!(has_route(
            &rails,
            "POST /accounts",
            "AccountsController::create"
        ));

        let laravel = php_routes(
            "Route::get('/users/{id}', [UserController::class, 'show']);\nRoute::resource('posts', PostController::class);",
        );
        assert!(has_route(
            &laravel,
            "GET /users/{id}",
            "UserController::show"
        ));
        assert!(has_route(
            &laravel,
            "DELETE /posts/:id",
            "PostController::destroy"
        ));
    }

    #[test]
    fn extracts_jvm_go_rust_dotnet_swift_and_play_routes() {
        let spring = java_routes(
            "@RequestMapping(\"/users\")\nclass UsersController {\n@GetMapping(\"/{id}\")\npublic User show() {}\n}",
        );
        assert!(has_route(
            &spring,
            "GET /users/{id}",
            "UsersController::show"
        ));

        let go = go_routes("router.GET(\"/users/:id\", users.Show)\nfunc Show() {}");
        assert!(has_route(&go, "GET /users/:id", "Show"));

        let rust = rust_routes(
            "Router::new().route(\"/users\", get(list).post(create));\nfn list() {}\nfn create() {}",
        );
        assert!(has_route(&rust, "GET /users", "list"));
        assert!(has_route(&rust, "POST /users", "create"));

        let csharp = csharp_routes(
            "[Route(\"api/[controller]\")]\npublic class UsersController {\n[HttpGet(\"{id}\")]\npublic User Show() {}\n}",
        );
        assert!(has_route(
            &csharp,
            "GET /api/Users/{id}",
            "UsersController::Show"
        ));

        let swift = swift_routes("app.post(\"users\", use: createUser)\nfunc createUser() {}");
        assert!(has_route(&swift, "POST /users", "createUser"));

        let play = play_routes(
            std::path::Path::new("conf/routes"),
            "GET /users/:id controllers.Users.show(id: Long)",
        );
        assert!(has_route(
            &play,
            "GET /users/:id",
            "controllers::Users::show"
        ));
    }

    #[test]
    fn extracts_react_and_native_client_bridges() {
        let react = react_routes(
            std::path::Path::new("src/App.tsx"),
            "<Route path=\"/dashboard\" element={<Dashboard />} />",
        );
        assert!(has_route(&react, "/dashboard", "Dashboard"));
        let next = react_routes(
            std::path::Path::new("app/users/[id]/page.tsx"),
            "export default function UserPage() {}",
        );
        assert!(has_route(&next, "/users/:id", "UserPage"));

        let fabric = fabric_typescript_components(
            "export default codegenNativeComponent<Props>('FancyView');",
        );
        assert!(
            fabric
                .iter()
                .any(|symbol| symbol.kind == "component" && symbol.label == "FancyView")
        );

        let objc = objective_c_bridge_symbols(
            "@implementation RCTCameraViewManager\nRCT_EXPORT_MODULE(Camera)\nRCT_EXPORT_METHOD(takePhoto:(id)resolve) {}\n@end",
        );
        assert!(
            objc.iter()
                .any(|symbol| symbol.kind == "method" && symbol.label == "takePhoto")
        );
        assert!(
            objc.iter()
                .any(|symbol| symbol.kind == "component" && symbol.label == "Camera")
        );

        let swift = swift_client_symbols(
            "struct DashboardView: View {}\npublic class BatteryModule: Module { Name(\"Battery\"); AsyncFunction(\"getLevel\") {} }",
        );
        assert!(
            swift
                .iter()
                .any(|symbol| symbol.kind == "component" && symbol.label == "DashboardView")
        );
        assert!(
            swift
                .iter()
                .any(|symbol| symbol.kind == "method" && symbol.label == "getLevel")
        );

        let kotlin = jvm_client_symbols(
            "class MapManager : SimpleViewManager<MapView>() { @ReactProp fun setZoom() {} }",
        );
        assert!(
            kotlin
                .iter()
                .any(|symbol| symbol.kind == "component" && symbol.label == "Map")
        );
    }
}
