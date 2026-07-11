//! Unit-level coverage for the new field instruction parser. These tests
//! pin behavior of `parse_field_instruction` for each Tier 1/2 field plus
//! the `Other` fallback for unknown field names.
//!
//! Invariants:
//! - A round-trip through `to_instruction_text` produces a string that
//!   parses back to the same semantic.
//! - Whitespace differences in the input do NOT produce different output.
//! - Tier 1/2 fields with malformed required arguments return `Err`.
//! - Unknown field names land in `Other`, never silently in `Toc`.

use stemma::domain::{
    DateTimeKind, FieldArg, FieldParseError, FieldSemantic, HyperlinkTarget, RefKind,
    parse_field_instruction,
};

#[test]
fn empty_instruction_errors() {
    assert_eq!(
        parse_field_instruction("").unwrap_err(),
        FieldParseError::EmptyInstruction
    );
    assert_eq!(
        parse_field_instruction("   ").unwrap_err(),
        FieldParseError::EmptyInstruction
    );
}

#[test]
fn hyperlink_url_only() {
    let s = parse_field_instruction(r#"HYPERLINK "https://example.com""#).unwrap();
    let h = match s {
        FieldSemantic::Hyperlink(h) => h,
        _ => panic!("expected Hyperlink"),
    };
    assert_eq!(
        h.target,
        HyperlinkTarget::Url {
            url: "https://example.com".to_string()
        }
    );
    assert!(h.tooltip.is_none());
    assert!(!h.no_history);
}

#[test]
fn hyperlink_bookmark_only() {
    let s = parse_field_instruction(r#"HYPERLINK \l "section_5""#).unwrap();
    match s {
        FieldSemantic::Hyperlink(h) => assert_eq!(
            h.target,
            HyperlinkTarget::Bookmark {
                anchor: "section_5".to_string()
            }
        ),
        _ => panic!("expected Hyperlink"),
    };
}

#[test]
fn hyperlink_url_with_anchor_and_tooltip() {
    let s =
        parse_field_instruction(r#"HYPERLINK "https://example.com" \l "frag" \o "tooltip text""#)
            .unwrap();
    match s {
        FieldSemantic::Hyperlink(h) => {
            assert_eq!(
                h.target,
                HyperlinkTarget::UrlWithBookmark {
                    url: "https://example.com".to_string(),
                    anchor: "frag".to_string()
                }
            );
            assert_eq!(h.tooltip.as_deref(), Some("tooltip text"));
        }
        _ => panic!("expected Hyperlink"),
    };
}

#[test]
fn hyperlink_missing_target_errors() {
    assert!(matches!(
        parse_field_instruction("HYPERLINK"),
        Err(FieldParseError::HyperlinkMissingTarget)
    ));
    assert!(matches!(
        parse_field_instruction(r"HYPERLINK \n \m"),
        Err(FieldParseError::HyperlinkMissingTarget)
    ));
}

#[test]
fn mergefield_basic() {
    let s = parse_field_instruction("MERGEFIELD CompanyName").unwrap();
    match s {
        FieldSemantic::MergeField(m) => assert_eq!(m.field_name, "CompanyName"),
        _ => panic!("expected MergeField"),
    }
}

#[test]
fn mergefield_with_format_switches() {
    let s = parse_field_instruction(r#"MERGEFIELD CompanyName \* Upper \* MERGEFORMAT"#).unwrap();
    let m = match s {
        FieldSemantic::MergeField(m) => m,
        _ => panic!("expected MergeField"),
    };
    assert_eq!(m.field_name, "CompanyName");
    // Last \* wins in our tokenizer (single slot per kind).
    assert_eq!(m.format.general.as_deref(), Some("MERGEFORMAT"));
}

#[test]
fn mergefield_whitespace_invariance() {
    let a = parse_field_instruction(r#"MERGEFIELD CompanyName \* MERGEFORMAT"#).unwrap();
    let b = parse_field_instruction(r#" MERGEFIELD   CompanyName    \* MERGEFORMAT  "#).unwrap();
    assert_eq!(a, b);
}

#[test]
fn mergefield_missing_name_errors() {
    assert_eq!(
        parse_field_instruction("MERGEFIELD").unwrap_err(),
        FieldParseError::MergeFieldMissingName
    );
}

#[test]
fn ref_pageref_noref_dispatch() {
    let r = parse_field_instruction("REF bookmark1").unwrap();
    let p = parse_field_instruction("PAGEREF bookmark1").unwrap();
    let n = parse_field_instruction("NOREF bookmark1").unwrap();
    let kind = |s: FieldSemantic| match s {
        FieldSemantic::Ref(r) => r.kind,
        _ => panic!("expected Ref"),
    };
    assert_eq!(kind(r), RefKind::Ref);
    assert_eq!(kind(p), RefKind::PageRef);
    assert_eq!(kind(n), RefKind::NoRef);
}

#[test]
fn ref_with_switches() {
    let s = parse_field_instruction(r"REF bookmark1 \h \n \w").unwrap();
    let r = match s {
        FieldSemantic::Ref(r) => r,
        _ => panic!("expected Ref"),
    };
    assert_eq!(r.bookmark, "bookmark1");
    assert!(r.insert_hyperlink);
    assert!(r.no_paragraph_number);
    assert!(r.paragraph_number_full);
    assert!(!r.paragraph_number_relative);
}

#[test]
fn ref_missing_bookmark_errors() {
    assert!(matches!(
        parse_field_instruction("REF"),
        Err(FieldParseError::RefMissingBookmark { .. })
    ));
}

#[test]
fn date_with_format_switch() {
    let s = parse_field_instruction(r#"DATE \@ "yyyy-MM-dd""#).unwrap();
    match s {
        FieldSemantic::DateTime(d) => {
            assert_eq!(d.kind, DateTimeKind::Date);
            assert_eq!(d.format.date_time.as_deref(), Some("yyyy-MM-dd"));
        }
        _ => panic!("expected DateTime"),
    }
}

#[test]
fn time_with_calendar_switches() {
    let s = parse_field_instruction(r"TIME \l \s").unwrap();
    match s {
        FieldSemantic::DateTime(d) => {
            assert_eq!(d.kind, DateTimeKind::Time);
            assert!(d.use_last_format);
            assert!(d.use_saka_era);
            assert!(!d.use_hijri);
        }
        _ => panic!("expected DateTime"),
    }
}

#[test]
fn if_basic() {
    let s = parse_field_instruction(r#"IF a > b "yes" "no""#).unwrap();
    let i = match s {
        FieldSemantic::If(i) => i,
        _ => panic!("expected If"),
    };
    assert!(i.expression_text.contains("a"));
    assert_eq!(i.true_text, "yes");
    assert_eq!(i.false_text, "no");
}

#[test]
fn if_missing_args_errors() {
    assert_eq!(
        parse_field_instruction("IF a > b").unwrap_err(),
        FieldParseError::IfMissingArgs
    );
    // Single-run IF fragment — also missing args.
    assert_eq!(
        parse_field_instruction("IF").unwrap_err(),
        FieldParseError::IfMissingArgs
    );
}

#[test]
fn formula_basic() {
    let s = parse_field_instruction("= 1 + 2").unwrap();
    match s {
        FieldSemantic::Formula(f) => assert_eq!(f.expression_text, "1 + 2"),
        _ => panic!("expected Formula"),
    }
}

#[test]
fn unknown_field_lands_in_other() {
    let s = parse_field_instruction(r#"SEQ Figure \* ARABIC"#).unwrap();
    let (name, args) = match s {
        FieldSemantic::Other {
            field_name,
            raw_args,
        } => (field_name, raw_args),
        _ => panic!("expected Other"),
    };
    assert_eq!(name, "SEQ");
    // SEQ has at least the bare "Figure" token.
    assert!(
        args.iter()
            .any(|a| matches!(a, FieldArg::Bare(s) if s == "Figure"))
    );
}

#[test]
fn toc_via_new_dispatcher() {
    let s = parse_field_instruction(r#"TOC \o "1-3" \h"#).unwrap();
    match s {
        FieldSemantic::Toc(t) => {
            assert_eq!(t.levels.from, 1);
            assert_eq!(t.levels.to, 3);
            assert!(t.include_hyperlinks);
        }
        _ => panic!("expected Toc"),
    }
}

#[test]
fn instruction_text_round_trip_hyperlink() {
    let original = parse_field_instruction(r#"HYPERLINK "https://example.com" \o "tip""#).unwrap();
    let serialized = original.to_instruction_text();
    let reparsed = parse_field_instruction(&serialized).unwrap();
    assert_eq!(original, reparsed);
}

#[test]
fn instruction_text_round_trip_mergefield() {
    let original = parse_field_instruction(r"MERGEFIELD CompanyName \b ': ' \* Upper").unwrap();
    let serialized = original.to_instruction_text();
    let reparsed = parse_field_instruction(&serialized).unwrap();
    assert_eq!(original, reparsed);
}

/// The diff and serializer pipelines both reach for
/// `semantic.to_instruction_text()` when present so that whitespace
/// shifts in the *raw* instruction string don't surface as content
/// changes. Pin the canonical form: two MERGEFIELDs that differ only in
/// whitespace must serialize to the same string.
#[test]
fn whitespace_variants_share_canonical_instruction_text() {
    let a = parse_field_instruction(r#"MERGEFIELD CompanyName \* MERGEFORMAT"#).unwrap();
    let b = parse_field_instruction(r#" MERGEFIELD   CompanyName    \* MERGEFORMAT  "#).unwrap();
    assert_eq!(a.to_instruction_text(), b.to_instruction_text());
}

/// HYPERLINK URL changes (with the same display text) must remain
/// visible structurally — the canonical form must reflect the URL
/// difference. This is the dual of the whitespace-invariance guarantee.
#[test]
fn hyperlink_url_changes_change_canonical_instruction_text() {
    let a = parse_field_instruction(r#"HYPERLINK "https://example.com""#).unwrap();
    let b = parse_field_instruction(r#"HYPERLINK "https://other.example""#).unwrap();
    assert_ne!(a.to_instruction_text(), b.to_instruction_text());
}
