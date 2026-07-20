//! Model-facing context budgeting primitives.

/// A complete unit of model-facing evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRecord {
    pub id: String,
    pub priority: i32,
    pub text: String,
    /// Source windows may discard complete trailing lines to fit a packet.
    /// Anchors, relations, and every other record remain indivisible.
    pub shrink_lines: bool,
}

/// The result of packing evidence into a bounded context packet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackedEvidence {
    pub records: Vec<EvidenceRecord>,
    pub estimated_tokens: usize,
    pub omitted: usize,
    pub next_offset: Option<usize>,
}

/// Conservatively estimates tokens without depending on a provider tokenizer.
///
/// Code and paths contain more punctuation than prose, so bytes-per-token alone
/// is not sufficient. The lexical estimate counts transitions into runs of
/// alphanumeric or punctuation characters, then keeps the larger estimate and
/// reserves five percent for provider variance.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let bytes = text.len().div_ceil(3);
    let mut segments = 0_usize;
    let mut previous = CharacterClass::Whitespace;
    for ch in text.chars() {
        let class = if ch.is_whitespace() {
            CharacterClass::Whitespace
        } else if ch.is_alphanumeric() || ch == '_' {
            CharacterClass::Lexical
        } else {
            CharacterClass::Punctuation
        };
        if class != CharacterClass::Whitespace && class != previous {
            segments += 1;
        }
        previous = class;
    }
    bytes.max(segments).saturating_mul(105).div_ceil(100)
}

/// Packs complete evidence records by priority and stable input order.
pub fn pack_evidence(
    records: impl IntoIterator<Item = EvidenceRecord>,
    token_budget: usize,
    offset: usize,
) -> PackedEvidence {
    let budget = token_budget.max(1);
    let mut ranked = records.into_iter().enumerate().collect::<Vec<_>>();
    ranked.sort_by_key(|(index, record)| (std::cmp::Reverse(record.priority), *index));
    let total = ranked.len();
    let mut packed = PackedEvidence::default();
    let mut next = None;
    for (rank, (_, mut record)) in ranked.into_iter().enumerate().skip(offset) {
        let remaining = budget.saturating_sub(packed.estimated_tokens);
        if record.shrink_lines {
            shrink_source_window(&mut record.text, remaining);
        }
        let tokens = estimate_tokens(&record.text);
        if packed.estimated_tokens + tokens <= budget {
            packed.estimated_tokens += tokens;
            packed.records.push(record);
        } else {
            next = Some(rank);
            packed.omitted += total.saturating_sub(rank);
            break;
        }
    }
    packed.omitted += offset.min(total);
    packed.next_offset = next;
    packed
}

fn shrink_source_window(text: &mut String, token_budget: usize) {
    if estimate_tokens(text) <= token_budget {
        return;
    }
    let mut lines = text.lines().collect::<Vec<_>>();
    while lines.len() > 2 && estimate_tokens(&lines.join("\n")) > token_budget {
        lines.pop();
    }
    *text = lines.join("\n");
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CharacterClass {
    Whitespace,
    Lexical,
    Punctuation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimator_is_conservative_for_code_and_unicode() {
        assert!(estimate_tokens("fn render_map(a: &str) -> Result<()> {") >= 12);
        assert!(estimate_tokens("🔐 secret") >= 4);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn packer_keeps_records_atomic_and_stable() {
        let records = [
            EvidenceRecord {
                id: "low".into(),
                priority: 1,
                text: "low priority evidence".into(),
                shrink_lines: false,
            },
            EvidenceRecord {
                id: "first".into(),
                priority: 10,
                text: "first anchor".into(),
                shrink_lines: false,
            },
            EvidenceRecord {
                id: "second".into(),
                priority: 10,
                text: "second anchor".into(),
                shrink_lines: false,
            },
        ];
        let budget = estimate_tokens("first anchor") + estimate_tokens("second anchor");
        let packed = pack_evidence(records, budget, 0);
        assert_eq!(
            packed
                .records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(packed.omitted, 1);
        assert_eq!(packed.next_offset, Some(2));
    }

    #[test]
    fn packer_shrinks_only_source_windows_by_complete_lines() {
        let record = EvidenceRecord {
            id: "source".into(),
            priority: 1,
            text: "S A1\n10\tfirst source line\n11\tsecond source line\n12\tthird source line"
                .into(),
            shrink_lines: true,
        };
        let budget = estimate_tokens("S A1\n10\tfirst source line");
        let packed = pack_evidence([record], budget, 0);
        assert_eq!(packed.records.len(), 1);
        assert_eq!(packed.records[0].text, "S A1\n10\tfirst source line");
    }
}
