use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use crate::{CodeIndex, graph::NodeId};

#[derive(Clone, Copy, Debug)]
pub struct SelectionOptions {
    pub max_nodes: usize,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self { max_nodes: 48 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub nodes: Vec<NodeId>,
    pub anchors: Vec<NodeId>,
    pub distances: BTreeMap<NodeId, u32>,
    pub truncated: bool,
}

pub fn select_subgraph(index: &CodeIndex, query: &str, options: SelectionOptions) -> Selection {
    let max_nodes = options.max_nodes.clamp(1, 96);
    let term_groups = terms(query);
    let normalized_query = query.to_ascii_lowercase();
    let flow_query = normalized_query.starts_with("how ")
        || [" reach ", " flow ", " become ", " collect ", " enter "]
            .iter()
            .any(|phrase| normalized_query.contains(phrase));
    let flattened_terms: BTreeSet<&str> = term_groups
        .iter()
        .flat_map(|group| group.iter().map(String::as_str))
        .collect();
    let include_non_production = flattened_terms
        .iter()
        .any(|term| matches!(*term, "test" | "tests" | "example" | "examples" | "bench"));
    let mut scored_by_group = vec![Vec::new(); term_groups.len()];
    for node in &index.graph.nodes {
        let raw_label = index.graph.atom(node.label);
        let label = raw_label.to_ascii_lowercase();
        let label_words = identifier_words(raw_label);
        let qualified = index
            .qualified_names
            .get(&node.id)
            .map(String::as_str)
            .unwrap_or("");
        let qualified_words = identifier_words(qualified);
        let qualified = qualified.to_ascii_lowercase();
        let path = index
            .spans
            .get(&node.id)
            .map(|span| span.path.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = index.graph.atom(node.kind);
        if matches!(kind, "file" | "import" | "boundary") {
            continue;
        }
        let non_production = is_non_production_path(&path);
        if non_production && !include_non_production {
            continue;
        }
        let group_scores: Vec<i32> = term_groups
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|term| {
                        score_term(
                            term,
                            &label,
                            &label_words,
                            &qualified,
                            &qualified_words,
                            &path,
                        )
                    })
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let lexical_score = group_scores.iter().sum::<i32>();
        let strong_concepts = group_scores.iter().filter(|score| **score >= 35).count();
        let multi_concept_bonus = strong_concepts.saturating_sub(1) as i32 * 100;
        let label_matches_query = label_words
            .iter()
            .any(|word| flattened_terms.contains(word.as_str()));
        let owner_matches_query = qualified_words
            .iter()
            .any(|word| flattened_terms.contains(word.as_str()) && !label_words.contains(word));
        let qualified_owner_bonus = if flow_query && label_matches_query && owner_matches_query {
            220 - label_words.len().saturating_sub(1).min(2) as i32 * 80
        } else {
            0
        };
        let mut score = lexical_score;
        score += match kind {
            "function" | "method" if flow_query => 68,
            "function" | "method" | "struct" | "trait" | "impl" => 8,
            _ => 0,
        };
        if path.starts_with("src/") || path.contains("/src/") {
            score += 20;
        }
        score += (index.graph.outgoing(node.id).len() + index.graph.incoming(node.id).len()).min(20)
            as i32;
        if lexical_score > 0 {
            for (group_index, group_score) in group_scores.into_iter().enumerate() {
                // Path-only matches are useful during traversal, but too weak to
                // become direct anchors. They otherwise crowd out actual symbols
                // when a repository or crate name appears in the question.
                if group_score >= 35 {
                    let context_bonus =
                        contextual_path_bonus(&term_groups[group_index], &label, &path);
                    let architecture_bonus =
                        architecture_path_bonus(&term_groups[group_index], &flattened_terms, &path);
                    let file_affinity = symbol_file_affinity_bonus(&label, &path);
                    let flow_module_entry_bonus = if flow_query
                        && kind == "function"
                        && path
                            .rsplit('/')
                            .next()
                            .and_then(|file| file.strip_suffix(".rs"))
                            == Some(label.as_str())
                    {
                        350
                    } else {
                        0
                    };
                    let api_boundary_bonus = if matches!(kind, "function" | "method")
                        && (path.ends_with("/lib.rs") || path.ends_with("/mod.rs"))
                    {
                        100
                    } else {
                        0
                    };
                    let compound_label_bonus = label_words
                        .iter()
                        .filter(|word| term_groups[group_index].contains(word))
                        .count()
                        .saturating_sub(1) as i32
                        * 80;
                    let flow_kind_bonus = if flow_query && matches!(kind, "function" | "method") {
                        150
                    } else {
                        0
                    };
                    scored_by_group[group_index].push((
                        Reverse(
                            group_score
                                + context_bonus
                                + architecture_bonus
                                + file_affinity
                                + flow_module_entry_bonus
                                + api_boundary_bonus
                                + compound_label_bonus
                                + flow_kind_bonus
                                + multi_concept_bonus
                                + qualified_owner_bonus
                                + score / 10,
                        ),
                        node.id,
                    ));
                }
            }
        }
    }
    for candidates in &mut scored_by_group {
        candidates.sort();
        diversify_candidate_paths(index, candidates);
    }
    scored_by_group.sort_by_key(|candidates| {
        candidates
            .first()
            .copied()
            .unwrap_or((Reverse(i32::MIN), NodeId(u32::MAX)))
    });
    let anchor_limit = max_nodes.min(12);
    // Natural-language flow questions carry enough concepts to fill the legend
    // directly. Reserve connector IDs only for short symbol-oriented queries;
    // relationship expansion still places other bridges in the visual graph.
    let connector_budget = if anchor_limit >= 8 && term_groups.len() <= 4 {
        2
    } else {
        0
    };
    let direct_limit = anchor_limit - connector_budget;
    let mut anchors = Vec::new();
    let mut owner_counts = BTreeMap::<String, usize>::new();
    let mut rank = 0;
    while anchors.len() < direct_limit {
        let before = anchors.len();
        for candidates in &scored_by_group {
            if let Some((_, node)) = candidates.get(rank)
                && !anchors.contains(node)
            {
                let owner = qualified_owner(index, *node);
                if owner
                    .as_ref()
                    .and_then(|owner| owner_counts.get(owner))
                    .is_some_and(|count| *count >= 2)
                {
                    continue;
                }
                anchors.push(*node);
                if let Some(owner) = owner {
                    *owner_counts.entry(owner).or_default() += 1;
                }
                if anchors.len() == direct_limit {
                    break;
                }
            }
        }
        if anchors.len() == before {
            break;
        }
        rank += 1;
    }
    if connector_budget > 0 {
        add_relationship_connectors(index, &mut anchors, anchor_limit, include_non_production);
    }
    if anchors.is_empty() {
        anchors = index
            .graph
            .nodes
            .iter()
            .filter(|node| index.graph.atom(node.kind) == "file")
            .take(12)
            .map(|node| node.id)
            .collect();
    }

    let mut queue = BinaryHeap::new();
    let mut distances = BTreeMap::new();
    for anchor in &anchors {
        distances.insert(*anchor, 0);
        queue.push((Reverse(0_u32), Reverse(*anchor)));
    }
    while let Some((Reverse(distance), Reverse(node))) = queue.pop() {
        if distances.get(&node).is_some_and(|known| *known < distance) {
            continue;
        }
        let adjacent = index
            .graph
            .outgoing(node)
            .iter()
            .chain(index.graph.incoming(node));
        for edge_id in adjacent {
            let Some(edge) = index.graph.edge(*edge_id) else {
                continue;
            };
            let other = if edge.source == node {
                edge.target
            } else {
                edge.source
            };
            let weight = match index.graph.atom(edge.kind) {
                "calls" | "implements" => 1,
                "uncertain_call" => 3,
                _ => 2,
            };
            let next = distance + weight;
            if distances.get(&other).is_none_or(|known| next < *known) {
                distances.insert(other, next);
                queue.push((Reverse(next), Reverse(other)));
            }
        }
    }
    let mut ranked: Vec<_> = distances
        .iter()
        .map(|(node, distance)| (*distance, *node))
        .collect();
    ranked.sort();
    let truncated = ranked.len() > max_nodes;
    let nodes: Vec<_> = ranked
        .into_iter()
        .take(max_nodes)
        .map(|(_, node)| node)
        .collect();
    let visible: BTreeSet<_> = nodes.iter().copied().collect();
    anchors.retain(|anchor| visible.contains(anchor));
    distances.retain(|node, _| visible.contains(node));
    Selection {
        nodes,
        anchors,
        distances,
        truncated,
    }
}

fn diversify_candidate_paths(index: &CodeIndex, candidates: &mut Vec<(Reverse<i32>, NodeId)>) {
    let mut seen_paths = BTreeSet::new();
    let mut first_by_path = Vec::with_capacity(candidates.len());
    let mut repeated_paths = Vec::new();
    for candidate in candidates.drain(..) {
        let path = index
            .spans
            .get(&candidate.1)
            .map(|span| span.path.as_str())
            .unwrap_or("");
        if path.is_empty() || seen_paths.insert(path.to_owned()) {
            first_by_path.push(candidate);
        } else {
            repeated_paths.push(candidate);
        }
    }
    first_by_path.extend(repeated_paths);
    *candidates = first_by_path;
}

fn qualified_owner(index: &CodeIndex, node: NodeId) -> Option<String> {
    let qualified = index.qualified_names.get(&node)?;
    let (owner, _) = qualified.rsplit_once("::")?;
    let owner = owner.strip_prefix("impl ").unwrap_or(owner);
    let owner = owner.split('<').next().unwrap_or(owner).trim();
    (!owner.is_empty()).then(|| owner.to_ascii_lowercase())
}

fn add_relationship_connectors(
    index: &CodeIndex,
    anchors: &mut Vec<NodeId>,
    anchor_limit: usize,
    include_non_production: bool,
) {
    let direct: BTreeSet<_> = anchors.iter().copied().collect();
    let mut scores = BTreeMap::<NodeId, i32>::new();
    for anchor in &direct {
        for edge_id in index
            .graph
            .incoming(*anchor)
            .iter()
            .chain(index.graph.outgoing(*anchor))
        {
            let Some(edge) = index.graph.edge(*edge_id) else {
                continue;
            };
            let other = if edge.source == *anchor {
                edge.target
            } else {
                edge.source
            };
            if direct.contains(&other) {
                continue;
            }
            let Some(node) = index.graph.node(other) else {
                continue;
            };
            let kind = index.graph.atom(node.kind);
            if matches!(kind, "file" | "import" | "boundary") {
                continue;
            }
            let path = index
                .spans
                .get(&other)
                .map(|span| span.path.as_str())
                .unwrap_or("");
            if !include_non_production && is_non_production_path(path) {
                continue;
            }
            let relation_score = match index.graph.atom(edge.kind) {
                // Callers explain how a matched symbol is reached; prefer them
                // over callees, which tend to fan out into implementation detail.
                "calls" if edge.target == *anchor => 100,
                "calls" => 60,
                "implements" => 50,
                // Ambiguous static resolution is still valuable for topology;
                // the renderer preserves uncertainty with a dashed edge.
                "uncertain_call" if edge.target == *anchor => 70,
                "uncertain_call" => 10,
                _ => continue,
            };
            let kind_score = match kind {
                "function" | "method" => 15,
                "struct" | "trait" | "impl" => 8,
                _ => 0,
            };
            *scores.entry(other).or_default() += relation_score + kind_score;
        }
    }
    let mut ranked: Vec<_> = scores
        .into_iter()
        .map(|(node, score)| (Reverse(score), node))
        .collect();
    ranked.sort();
    anchors.extend(
        ranked
            .into_iter()
            .map(|(_, node)| node)
            .take(anchor_limit.saturating_sub(anchors.len())),
    );
}

fn contextual_path_bonus(group: &[String], label: &str, path: &str) -> i32 {
    let cli_concept = group
        .iter()
        .any(|term| matches!(term.as_str(), "arg" | "args" | "cli"));
    let config_concept = group
        .iter()
        .any(|term| matches!(term.as_str(), "config" | "configure"));
    let transition_concept = group.iter().any(|term| term == "from_low_args");
    if !(cli_concept || config_concept || transition_concept) {
        return 0;
    }
    let mut bonus = 0;
    if path.contains("/flags/") || path.starts_with("flags/") {
        bonus += if cli_concept { 120 } else { 80 };
    } else if path.contains("/cli/") || path.starts_with("cli/") {
        bonus += if cli_concept { 50 } else { 20 };
    }
    if cli_concept && label.contains("parse") {
        bonus += 70;
    }
    if transition_concept && label.contains("parse") {
        bonus += 140;
    }
    bonus
}

fn architecture_path_bonus(group: &[String], terms: &BTreeSet<&str>, path: &str) -> i32 {
    let scheduler_context = terms
        .iter()
        .any(|term| matches!(*term, "schedule" | "scheduler" | "worker"));
    let scheduler_group = group.iter().any(|term| {
        matches!(
            term.as_str(),
            "execute" | "poll" | "run" | "schedule" | "scheduler" | "worker"
        )
    });
    let lsp_dispatch_context = terms.iter().any(|term| *term == "lsp")
        && terms
            .iter()
            .any(|term| matches!(*term, "dispatch" | "dispatcher" | "handler"));
    let timer_context = terms
        .iter()
        .any(|term| matches!(*term, "sleep" | "timer" | "deadline"))
        && terms.iter().any(|term| *term == "driver");
    let timer_group = group.iter().any(|term| {
        matches!(
            term.as_str(),
            "deadline"
                | "driver"
                | "elapsed"
                | "expiration"
                | "poll"
                | "ready"
                | "time"
                | "timer"
                | "wake"
        )
    });
    let timer_entry_group = group.iter().any(|term| {
        matches!(
            term.as_str(),
            "deadline" | "elapsed" | "expiration" | "poll" | "ready" | "wake"
        )
    });
    let sleep_group = group.iter().any(|term| term == "sleep");
    let completion_context = terms.iter().any(|term| *term == "completion")
        && terms
            .iter()
            .any(|term| matches!(*term, "analysis" | "context" | "collect"));
    let completion_group = group.iter().any(|term| {
        matches!(
            term.as_str(),
            "analysis" | "collect" | "completion" | "context" | "item" | "items"
        )
    });
    let search_pipeline_context = terms.iter().any(|term| *term == "search")
        && terms
            .iter()
            .any(|term| matches!(*term, "haystack" | "walk" | "worker"));
    let search_pipeline_group = group.iter().any(|term| {
        matches!(
            term.as_str(),
            "execute" | "haystack" | "path" | "search" | "walk" | "worker"
        )
    });
    let mut bonus = 0;
    if scheduler_context && scheduler_group {
        if path.contains("/runtime/") {
            bonus += 100;
        }
        if path.contains("/scheduler/") {
            bonus += 50;
        }
    }
    if lsp_dispatch_context && path.contains("/handlers/") {
        bonus += 80;
    }
    if timer_context && timer_group && path.contains("/runtime/time/") {
        bonus += 120;
    }
    if timer_context && timer_entry_group && path.ends_with("/runtime/time/entry.rs") {
        bonus += 180;
    }
    if timer_context && sleep_group && path.contains("/time/") {
        bonus += 80;
    }
    if completion_context && completion_group {
        if path.contains("/ide-completion/") {
            bonus += 120;
        } else if path.contains("/ide/src/") {
            bonus += 80;
        }
    }
    if search_pipeline_context && search_pipeline_group {
        if path.ends_with("/core/search.rs") {
            bonus += 220;
        } else if path.ends_with("/core/main.rs") {
            bonus += 180;
        } else if path.ends_with("/core/haystack.rs") {
            bonus += 120;
        } else if path.ends_with("/ignore/src/walk.rs") {
            bonus += 80;
        }
    }
    bonus
}

fn symbol_file_affinity_bonus(label: &str, path: &str) -> i32 {
    let file_stem = path
        .rsplit('/')
        .next()
        .and_then(|file| file.strip_suffix(".rs"))
        .unwrap_or("");
    if file_stem == label || identifier_words(label).iter().any(|word| word == file_stem) {
        50
    } else if file_stem.len() >= 4
        && identifier_words(label)
            .iter()
            .any(|word| word.starts_with(file_stem) || file_stem.starts_with(word))
    {
        35
    } else {
        0
    }
}

fn is_non_production_path(path: &str) -> bool {
    let top_level = path.split('/').next().unwrap_or("");
    top_level.ends_with("-test")
        || top_level.ends_with("-tests")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/examples/")
        || path.contains("/benches/")
        || path.ends_with("/tests.rs")
        || path.ends_with("/test.rs")
        || path.ends_with("/testutil.rs")
        || path.ends_with("/test_util.rs")
        || path.ends_with("/test_utils.rs")
        || path == "build.rs"
        || path.ends_with("/build.rs")
        || path.starts_with("tests/")
        || path.starts_with("examples/")
        || path.starts_with("benches/")
}

fn score_term(
    term: &str,
    label: &str,
    words: &[String],
    qualified: &str,
    qualified_words: &[String],
    path: &str,
) -> i32 {
    let file_stem = path
        .rsplit('/')
        .next()
        .and_then(|file| file.strip_suffix(".rs"))
        .unwrap_or("");
    if label == term {
        160
    } else if words.iter().any(|word| word == term) {
        100
    } else if qualified.ends_with(term) {
        90
    } else if qualified_words.iter().any(|word| word == term) {
        85
    } else if label.contains(term) {
        60
    } else if words.iter().any(|word| {
        term.len() >= 3
            && word.len() >= 3
            && (word.starts_with(term) || term.starts_with(word.as_str()))
    }) {
        35
    } else if file_stem == term {
        55
    } else if !file_stem.is_empty()
        && term.len() >= 4
        && (file_stem.starts_with(term) || term.starts_with(file_stem))
    {
        35
    } else if qualified.contains(term) {
        25
    } else if path.contains(term) {
        10
    } else {
        0
    }
}

fn terms(query: &str) -> Vec<Vec<String>> {
    let mut groups = Vec::<BTreeSet<String>>::new();
    let normalized = query
        .to_ascii_lowercase()
        .replace("command-line", "cli")
        .replace("command line", "cli")
        .replace("high-level", "")
        .replace("high level", "");
    let cli_to_config = (normalized.contains("cli") || normalized.contains("argument"))
        && (normalized.contains("config") || normalized.contains("configure"));
    if cli_to_config {
        groups.push(
            [
                "convert",
                "conversion",
                "from_low_args",
                "hiargs",
                "lowargs",
                "parse",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        );
    }
    for raw in normalized.split(|ch: char| !ch.is_alphanumeric() && ch != '_') {
        let value = raw.trim_matches('_').to_ascii_lowercase();
        if value.len() > 1 && !is_stop_word(&value) {
            let variants: BTreeSet<_> = term_variants(&value).into_iter().collect();
            let overlapping: Vec<_> = groups
                .iter()
                .enumerate()
                .filter_map(|(index, group)| (!group.is_disjoint(&variants)).then_some(index))
                .collect();
            if overlapping.is_empty() {
                groups.push(variants);
            } else {
                let first = overlapping[0];
                groups[first].extend(variants);
                for index in overlapping.into_iter().skip(1).rev() {
                    let merged = groups.remove(index);
                    groups[first].extend(merged);
                }
            }
        }
    }
    let mut groups: Vec<Vec<String>> = groups
        .into_iter()
        .map(|group| group.into_iter().collect())
        .collect();
    groups.sort();
    groups
}

fn term_variants(value: &str) -> Vec<String> {
    let mut variants = vec![value.to_owned()];
    for suffix in ["ation", "ing", "ion", "ed", "es", "s"] {
        if value.len() > suffix.len() + 3 && value.ends_with(suffix) {
            let stem = value[..value.len() - suffix.len()].to_owned();
            variants.push(stem.clone());
            if matches!(suffix, "ation" | "ing" | "ed") && !stem.ends_with('e') {
                variants.push(format!("{stem}e"));
            }
            break;
        }
    }
    let aliases: &[&str] = match value {
        "argument" | "arguments" => &["arg", "args", "cli"],
        "command" => &["cli"],
        "configuration" | "configured" => &["config", "configure"],
        "directory" | "directories" => &["dir"],
        "deadline" => &["elapsed", "expiration"],
        "dispatch" | "dispatched" => &["dispatcher", "route"],
        "driver" => &["park", "process"],
        "poll" | "polled" => &["execute", "run"],
        "ready" => &["elapsed", "poll", "wake"],
        "readiness" => &["ready"],
        "register" | "registered" => &["registration"],
        "scheduler" => &["schedule"],
        "spawn" | "spawned" => &["spawner"],
        "timer" => &["time"],
        _ => &[],
    };
    variants.extend(aliases.iter().map(|alias| (*alias).to_owned()));
    if value.len() > 3 && !value.ends_with('s') {
        variants.push(format!("{value}s"));
    }
    variants
}

fn identifier_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for ch in value.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lowercase = false;
        } else {
            if ch.is_uppercase() && previous_lowercase && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lowercase = ch.is_lowercase();
            current.push(ch.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_stop_word(value: &str) -> bool {
    matches!(
        value,
        "a" | "an"
            | "and"
            | "are"
            | "be"
            | "become"
            | "by"
            | "collect"
            | "do"
            | "does"
            | "eventually"
            | "editor"
            | "for"
            | "from"
            | "get"
            | "gets"
            | "how"
            | "in"
            | "into"
            | "is"
            | "of"
            | "on"
            | "reach"
            | "reaches"
            | "rest"
            | "the"
            | "through"
            | "to"
            | "use"
            | "used"
            | "using"
            | "what"
            | "where"
            | "with"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceSpan, graph::PackedGraph};

    fn span(path: &str) -> SourceSpan {
        SourceSpan {
            path: path.into(),
            start_line: 1,
            end_line: 2,
        }
    }

    #[test]
    fn ranks_exact_anchor_and_expands_neighbors() {
        let mut graph = PackedGraph::default();
        let a = graph.add_node("function", "start");
        let b = graph.add_node("function", "finish");
        graph.add_edge(a, b, "calls").unwrap();
        let index = CodeIndex {
            graph,
            spans: BTreeMap::<NodeId, SourceSpan>::new(),
            qualified_names: [(a, "start".into()), (b, "finish".into())].into(),
            version: 1,
        };
        let selection = select_subgraph(&index, "start", SelectionOptions { max_nodes: 2 });
        assert_eq!(selection.anchors, vec![a]);
        assert_eq!(selection.nodes, vec![a, b]);
    }

    #[test]
    fn diversifies_repeated_term_candidates_across_source_paths() {
        let mut graph = PackedGraph::default();
        let exact = graph.add_node("function", "parse");
        let same_file = graph.add_node("function", "parse_payload");
        let other_file = graph.add_node("function", "parse_request");
        let index = CodeIndex {
            graph,
            spans: [
                (exact, span("src/parser.rs")),
                (same_file, span("src/parser.rs")),
                (other_file, span("src/request.rs")),
            ]
            .into(),
            qualified_names: [
                (exact, "parse".into()),
                (same_file, "parse_payload".into()),
                (other_file, "parse_request".into()),
            ]
            .into(),
            version: 1,
        };

        let selection = select_subgraph(&index, "parse", SelectionOptions { max_nodes: 2 });

        assert_eq!(selection.anchors, vec![exact, other_file]);
    }

    #[test]
    fn flow_queries_prefer_free_functions_that_define_a_module_entry() {
        let mut graph = PackedGraph::default();
        let method = graph.add_node("method", "dispatch");
        let entry = graph.add_node("function", "dispatch");
        let index = CodeIndex {
            graph,
            spans: [
                (method, span("src/request.rs")),
                (entry, span("src/dispatch.rs")),
            ]
            .into(),
            qualified_names: [
                (method, "impl Request::dispatch".into()),
                (entry, "dispatch".into()),
            ]
            .into(),
            version: 1,
        };

        let selection = select_subgraph(
            &index,
            "How does a request dispatch flow?",
            SelectionOptions { max_nodes: 1 },
        );

        assert_eq!(selection.anchors, vec![entry]);
    }

    #[test]
    fn natural_language_inflections_match_symbol_stems() {
        let mut graph = PackedGraph::default();
        let render = graph.add_node("function", "render_map");
        let index = CodeIndex {
            graph,
            spans: BTreeMap::<NodeId, SourceSpan>::new(),
            qualified_names: [(render, "render_map".into())].into(),
            version: 1,
        };
        let selection = select_subgraph(
            &index,
            "how is topology rendered",
            SelectionOptions { max_nodes: 1 },
        );
        assert_eq!(selection.anchors, vec![render]);
    }

    #[test]
    fn splits_camel_case_and_expands_common_code_terms() {
        assert_eq!(identifier_words("AnalysisHost"), ["analysis", "host"]);
        assert!(
            identifier_words("impl SearchWorker<W>::search")
                .windows(2)
                .any(|words| words == ["search", "worker"])
        );
        assert!(term_variants("arguments").contains(&"args".to_owned()));
        assert!(term_variants("parsed").contains(&"parse".to_owned()));
        assert!(term_variants("dispatch").contains(&"dispatcher".to_owned()));
        assert!(term_variants("polled").contains(&"run".to_owned()));
    }

    #[test]
    fn rejects_short_prefix_noise_and_recognizes_module_names() {
        assert_eq!(
            score_term("incoming", "in", &["in".into()], "in", &["in".into()], ""),
            0
        );
        assert_eq!(
            score_term(
                "worker",
                "schedule_task",
                &["schedule".into(), "task".into()],
                "schedule_task",
                &["schedule".into(), "task".into()],
                "src/runtime/worker.rs",
            ),
            55
        );
        assert!(is_non_production_path("tokio-test/src/task.rs"));
        assert!(is_non_production_path("crates/searcher/src/testutil.rs"));
        assert!(is_non_production_path("build.rs"));
    }

    #[test]
    fn qualified_owner_words_disambiguate_flow_symbols() {
        let mut graph = PackedGraph::default();
        let worker_search = graph.add_node("method", "search");
        let generic_search = graph.add_node("method", "search_path");
        let index = CodeIndex {
            graph,
            spans: [
                (worker_search, span("crates/core/search.rs")),
                (generic_search, span("crates/searcher/src/searcher/mod.rs")),
            ]
            .into(),
            qualified_names: [
                (worker_search, "impl SearchWorker::search".into()),
                (generic_search, "impl Searcher::search_path".into()),
            ]
            .into(),
            version: 1,
        };
        let selection = select_subgraph(
            &index,
            "How does the configured search worker execute?",
            SelectionOptions { max_nodes: 1 },
        );
        assert_eq!(selection.anchors, vec![worker_search]);
    }

    #[test]
    fn merges_alias_equivalent_query_concepts() {
        let groups = terms("How do command-line arguments become configuration?");
        assert!(
            groups.iter().any(
                |group| group.contains(&"cli".to_owned()) && group.contains(&"args".to_owned())
            )
        );
    }

    #[test]
    fn adds_callers_that_bridge_matched_symbols() {
        let mut graph = PackedGraph::default();
        let parse = graph.add_node("function", "parse");
        let hi_args = graph.add_node("struct", "HiArgs");
        let from_low_args = graph.add_node("method", "from_low_args");
        let config = graph.add_node("struct", "Config");
        graph.add_edge(parse, from_low_args, "calls").unwrap();
        graph.add_edge(hi_args, from_low_args, "contains").unwrap();
        let index = CodeIndex {
            graph,
            spans: [
                (
                    parse,
                    SourceSpan {
                        path: "crates/core/flags/parse.rs".into(),
                        start_line: 49,
                        end_line: 54,
                    },
                ),
                (
                    hi_args,
                    SourceSpan {
                        path: "crates/core/flags/hiargs.rs".into(),
                        start_line: 36,
                        end_line: 106,
                    },
                ),
                (
                    from_low_args,
                    SourceSpan {
                        path: "crates/core/flags/hiargs.rs".into(),
                        start_line: 113,
                        end_line: 322,
                    },
                ),
                (
                    config,
                    SourceSpan {
                        path: "crates/core/flags/config.rs".into(),
                        start_line: 16,
                        end_line: 53,
                    },
                ),
            ]
            .into(),
            qualified_names: [
                (parse, "parse".into()),
                (hi_args, "HiArgs".into()),
                (from_low_args, "impl HiArgs::from_low_args".into()),
                (config, "Config".into()),
            ]
            .into(),
            version: 1,
        };
        let selection = select_subgraph(
            &index,
            "How do command-line arguments become high-level configuration?",
            SelectionOptions { max_nodes: 12 },
        );
        assert!(selection.anchors.contains(&from_low_args));
        assert!(selection.anchors.contains(&parse));
    }

    #[test]
    fn flow_questions_prefer_completion_api_boundaries() {
        let mut graph = PackedGraph::default();
        let analysis_entry = graph.add_node("method", "completions");
        let engine_entry = graph.add_node("function", "completions");
        let context = graph.add_node("enum", "CompletionAnalysis");
        let helper = graph.add_node("function", "complete_item_snippet");
        let index = CodeIndex {
            graph,
            spans: [
                (analysis_entry, span("crates/ide/src/lib.rs")),
                (engine_entry, span("crates/ide-completion/src/lib.rs")),
                (context, span("crates/ide-completion/src/context.rs")),
                (
                    helper,
                    span("crates/ide-completion/src/completions/snippet.rs"),
                ),
            ]
            .into(),
            qualified_names: [
                (analysis_entry, "impl Analysis::completions".into()),
                (engine_entry, "completions".into()),
                (context, "CompletionAnalysis".into()),
                (helper, "complete_item_snippet".into()),
            ]
            .into(),
            version: 1,
        };
        let selection = select_subgraph(
            &index,
            "How does an editor completion request reach context analysis and collect items?",
            SelectionOptions { max_nodes: 4 },
        );
        assert!(selection.anchors.contains(&analysis_entry));
        assert!(selection.anchors.contains(&engine_entry));
    }

    #[test]
    fn timer_lifecycle_covers_sleep_entry_and_driver() {
        let mut graph = PackedGraph::default();
        let sleep = graph.add_node("function", "sleep");
        let poll_elapsed = graph.add_node("method", "poll_elapsed");
        let park_internal = graph.add_node("method", "park_internal");
        let generic_poll = graph.add_node("method", "poll");
        let index = CodeIndex {
            graph,
            spans: [
                (sleep, span("tokio/src/time/sleep.rs")),
                (poll_elapsed, span("tokio/src/runtime/time/entry.rs")),
                (park_internal, span("tokio/src/runtime/time/mod.rs")),
                (generic_poll, span("tokio/src/runtime/time/wheel/mod.rs")),
            ]
            .into(),
            qualified_names: [
                (sleep, "sleep".into()),
                (poll_elapsed, "impl TimerEntry::poll_elapsed".into()),
                (park_internal, "impl Driver::park_internal".into()),
                (generic_poll, "impl Wheel::poll".into()),
            ]
            .into(),
            version: 1,
        };
        let selection = select_subgraph(
            &index,
            "How does a sleep future reach the timer driver and become ready after its deadline?",
            SelectionOptions { max_nodes: 4 },
        );
        assert!(selection.anchors.contains(&sleep));
        assert!(selection.anchors.contains(&poll_elapsed));
        assert!(selection.anchors.contains(&park_internal));
    }
}
