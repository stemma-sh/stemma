//! Numbering synthesis for Word documents.
//!
//! Parses `word/numbering.xml` and synthesizes visible number text (e.g., "1.", "(a)")
//! for paragraphs that use Word's auto-numbering feature.

use std::collections::HashMap;

use serde::Serialize;
use xmltree::{Element, XMLNode};

use crate::xml_attrs::attr_get;

const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// How a paragraph's numbering is sourced.
///
/// Either Word's auto-numbering machinery (`w:numPr` driving counters that the
/// IR carries through) or a manually-typed literal prefix (e.g., "(a)").
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberingSource {
    /// Word auto-numbering via w:numPr — IR handles continuation.
    Auto,
    /// Manually-typed prefix (e.g., literal "(a)") — consumer must include
    /// the number in the visible text when synthesizing content.
    LiteralPrefix,
}

/// Error type for numbering parsing and synthesis.
#[derive(Debug)]
pub enum NumberingError {
    XmlParse(String),
    MissingAbstractNum { num_id: u32 },
    MissingLevel { abstract_num_id: u32, ilvl: u32 },
}

impl std::fmt::Display for NumberingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NumberingError::XmlParse(msg) => write!(f, "numbering XML parse error: {msg}"),
            NumberingError::MissingAbstractNum { num_id } => {
                write!(f, "numId {num_id} references unknown abstractNumId")
            }
            NumberingError::MissingLevel {
                abstract_num_id,
                ilvl,
            } => {
                write!(f, "abstractNumId {abstract_num_id} missing level {ilvl}")
            }
        }
    }
}

impl std::error::Error for NumberingError {}

/// Number format type from Word's numFmt element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumFormat {
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
    None,
    Unknown(String),
}

impl NumFormat {
    fn from_str(s: &str) -> Self {
        match s {
            "decimal" => NumFormat::Decimal,
            "lowerLetter" => NumFormat::LowerLetter,
            "upperLetter" => NumFormat::UpperLetter,
            "lowerRoman" => NumFormat::LowerRoman,
            "upperRoman" => NumFormat::UpperRoman,
            "bullet" => NumFormat::Bullet,
            "none" => NumFormat::None,
            other => NumFormat::Unknown(other.to_string()),
        }
    }

    /// Returns true if this format produces visible text (not a bullet).
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            NumFormat::Decimal
                | NumFormat::LowerLetter
                | NumFormat::UpperLetter
                | NumFormat::LowerRoman
                | NumFormat::UpperRoman
        )
    }
}

/// Separator between the numbering text and the paragraph body (§17.9.28).
///
/// Controls the character inserted after the synthesized number text:
/// - Tab (default when omitted): inserts a tab character
/// - Space: inserts a single space
/// - Nothing: no separator
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LevelSuffix {
    Tab,
    Space,
    Nothing,
}

impl Default for LevelSuffix {
    /// §17.9.28: "If this element is omitted, then the suff value shall be
    /// assumed to be tab."
    fn default() -> Self {
        LevelSuffix::Tab
    }
}

impl LevelSuffix {
    fn from_str(s: &str) -> Self {
        match s {
            "space" => LevelSuffix::Space,
            "nothing" => LevelSuffix::Nothing,
            // "tab" or any unrecognized value falls back to the spec default
            _ => LevelSuffix::Tab,
        }
    }

    /// Returns the separator string for this suffix type.
    pub fn separator(&self) -> &'static str {
        match self {
            LevelSuffix::Tab => "\t",
            LevelSuffix::Space => " ",
            LevelSuffix::Nothing => "",
        }
    }
}

/// Indentation properties from a numbering level's `w:pPr/w:ind` (§17.9.22).
///
/// These apply to any paragraph referencing this level, unless the paragraph
/// specifies its own direct `w:ind`.
#[derive(Clone, Debug, Default)]
pub struct LevelIndent {
    pub left: Option<i32>,
    pub right: Option<i32>,
    pub effective_first_line_twips: Option<i32>,
}

/// Level definition within an abstract numbering scheme.
#[derive(Clone, Debug)]
pub struct LevelDef {
    pub ilvl: u32,
    pub num_fmt: NumFormat,
    pub start: u32,
    pub lvl_text: String,
    /// When true, all %N references in lvlText are formatted as decimal,
    /// regardless of the referenced level's numFmt (§17.9.4).
    pub is_legal: bool,
    /// Controls when this level's counter resets (§17.9.10).
    /// - None: default behavior (restart when the immediately previous level is encountered)
    /// - Some(0): never restart
    /// - Some(n): restart when level n-1 (0-indexed) is encountered
    pub restart_level: Option<u32>,
    /// Paragraph indentation from `w:lvl/w:pPr/w:ind` (§17.9.22).
    /// Acts as a base layer: direct paragraph indent overrides per-field.
    pub indent: Option<LevelIndent>,
    /// Reverse binding: paragraph style ID that this level claims (§17.9.23).
    /// When a paragraph uses this style, it gets numbering from this level,
    /// even if the style itself has no numPr.
    pub p_style: Option<String>,
    /// Separator between the number text and the paragraph body (§17.9.28).
    /// Defaults to Tab when the `w:suff` element is omitted.
    pub suffix: LevelSuffix,
}

/// Abstract numbering definition (w:abstractNum).
#[derive(Clone, Debug)]
pub struct AbstractNum {
    pub abstract_num_id: u32,
    pub levels: HashMap<u32, LevelDef>,
    /// §17.9.21: references a numbering style whose numPr provides the actual
    /// numbering definition. When present, this abstractNum typically has no
    /// levels — the levels come from the abstractNum with a matching `styleLink`.
    pub num_style_link: Option<String>,
    /// §17.9.27: declares that this abstractNum provides the levels for a
    /// numbering style. Used as the target of `numStyleLink` references.
    pub style_link: Option<String>,
}

/// Override for a specific level within a numbering instance (§17.9.8).
///
/// A lvlOverride can contain a full level replacement, a startOverride, or both.
#[derive(Clone, Debug)]
pub struct LevelOverride {
    /// Full replacement level definition, if present.
    pub level: Option<LevelDef>,
    /// Override the starting value for this level (§17.9.26).
    pub start_override: Option<u32>,
}

/// Concrete numbering instance (w:num) that references an abstractNum.
#[derive(Clone, Debug)]
pub struct NumInstance {
    pub num_id: u32,
    pub abstract_num_id: u32,
    /// Per-level overrides from w:lvlOverride elements (§17.9.8).
    pub level_overrides: HashMap<u32, LevelOverride>,
}

/// Parsed numbering definitions from word/numbering.xml.
#[derive(Clone, Debug, Default)]
pub struct NumberingDefinitions {
    pub abstract_nums: HashMap<u32, AbstractNum>,
    pub num_instances: HashMap<u32, NumInstance>,
}

impl NumberingDefinitions {
    /// Parse numbering definitions from XML bytes.
    pub fn parse(xml_bytes: &[u8]) -> Result<Self, String> {
        if xml_bytes.is_empty() {
            return Err("word/numbering.xml is empty".to_string());
        }

        let root = crate::word_xml::parse_document_xml(xml_bytes)
            .map_err(|err| format!("failed to parse word/numbering.xml: {err:?}"))?;

        let mut abstract_nums = HashMap::new();
        let mut num_instances = HashMap::new();

        // Parse w:abstractNum elements
        for child in &root.children {
            let element = match child {
                XMLNode::Element(el) => el,
                _ => continue,
            };

            if is_w_tag(element, "abstractNum") {
                if let Some(abstract_num) = parse_abstract_num(element) {
                    abstract_nums.insert(abstract_num.abstract_num_id, abstract_num);
                }
            } else if is_w_tag(element, "num")
                && let Some(num_instance) = parse_num_instance(element)
            {
                num_instances.insert(num_instance.num_id, num_instance);
            }
        }

        Ok(NumberingDefinitions {
            abstract_nums,
            num_instances,
        })
    }

    /// Look up the level definition for a given numId and ilvl.
    ///
    /// Checks level overrides on the num instance first (full lvl replacement),
    /// then falls back to the abstract numbering definition.
    /// If the abstract definition has a `numStyleLink` (§17.9.21), follows the
    /// chain to find the abstractNum with a matching `styleLink` (§17.9.27).
    pub fn get_level(&self, num_id: u32, ilvl: u32) -> Option<&LevelDef> {
        let num_instance = self.num_instances.get(&num_id)?;
        // Check for a full level override first
        if let Some(ovr) = num_instance.level_overrides.get(&ilvl)
            && let Some(ref level) = ovr.level
        {
            return Some(level);
        }
        let abstract_num = self.abstract_nums.get(&num_instance.abstract_num_id)?;

        // Try the direct level first
        if let Some(level) = abstract_num.levels.get(&ilvl) {
            return Some(level);
        }

        // §17.9.21 / §17.9.27: If this abstractNum has a numStyleLink but no
        // levels, find the abstractNum with a matching styleLink and use its levels.
        if let Some(ref link_name) = abstract_num.num_style_link {
            let target = self
                .abstract_nums
                .values()
                .find(|an| an.style_link.as_deref() == Some(link_name.as_str()))?;
            return target.levels.get(&ilvl);
        }

        None
    }

    /// Build a reverse mapping from paragraph style IDs to numbering (num_id, ilvl).
    ///
    /// Per §17.9.23: when an abstract numbering level has `<w:pStyle w:val="X"/>`,
    /// any paragraph with style "X" gets numbering from that level — even if the
    /// style itself has no numPr.
    ///
    /// The returned num_id is the concrete `w:num` instance that references the
    /// abstractNum containing the pStyle binding.
    pub fn build_pstyle_reverse_map(&self) -> HashMap<String, (u32, u32)> {
        let mut map = HashMap::new();

        // The spec (§17.9.23) doesn't define priority when several num instances
        // (or several levels) bind the same pStyle, but "first claim wins" over
        // hash-map iteration order would resolve a paragraph's numbering — and
        // thus the numPr that reaches the wire on a rebuild — nondeterministically
        // across processes. Walk num instances by ascending numId, and each
        // abstract's levels by ascending ilvl, so the lowest (numId, ilvl) claim
        // wins deterministically (H1).
        let mut num_instances: Vec<&NumInstance> = self.num_instances.values().collect();
        num_instances.sort_by_key(|ni| ni.num_id);
        for num_instance in num_instances {
            let Some(abstract_num) = self.abstract_nums.get(&num_instance.abstract_num_id) else {
                continue;
            };
            let mut levels: Vec<&LevelDef> = abstract_num.levels.values().collect();
            levels.sort_by_key(|l| l.ilvl);
            for level in levels {
                if let Some(ref style_id) = level.p_style {
                    map.entry(style_id.clone())
                        .or_insert((num_instance.num_id, level.ilvl));
                }
            }
        }

        map
    }

    /// Look up the startOverride for a given numId and ilvl, if any.
    fn get_start_override(&self, num_id: u32, ilvl: u32) -> Option<u32> {
        let num_instance = self.num_instances.get(&num_id)?;
        num_instance
            .level_overrides
            .get(&ilvl)
            .and_then(|ovr| ovr.start_override)
    }

    /// The effective starting value for a level's counter, applying MS-OI §2.1.292:
    /// a `start` element that is a child of an override `lvl` is IGNORED by Word.
    /// So when the level definition is supplied by a `lvlOverride` full replacement,
    /// the override lvl's own start does NOT govern — fall back to the abstract
    /// level's start. (`startOverride` §17.9.26 is applied separately by the caller
    /// and takes precedence.) The exact value when no abstract level exists is
    /// unsettled (pending confirmation against real Word); we use the spec-default start of 0.
    fn effective_start(&self, num_id: u32, ilvl: u32) -> u32 {
        let Some(num_instance) = self.num_instances.get(&num_id) else {
            return self.get_level(num_id, ilvl).map_or(0, |l| l.start);
        };
        let from_override = num_instance
            .level_overrides
            .get(&ilvl)
            .and_then(|o| o.level.as_ref())
            .is_some();
        if from_override {
            // §2.1.292: drop the override lvl's own start; use the abstract level's.
            return self
                .abstract_nums
                .get(&num_instance.abstract_num_id)
                .and_then(|abs| abs.levels.get(&ilvl))
                .map_or(0, |abs_lvl| abs_lvl.start);
        }
        self.get_level(num_id, ilvl).map_or(0, |l| l.start)
    }
}

/// Tracks counter state during document traversal for numbering synthesis.
#[derive(Clone, Debug, Default)]
pub struct NumberingState {
    /// Counter values keyed by (numId, ilvl).
    /// Each counter tracks the current value for that numbering level.
    /// Uses i64 because the pre-increment initialization (`start - 1`) can
    /// underflow u32 when start is 0 (the spec default per §17.9.25).
    counters: HashMap<(u32, u32), i64>,
    /// Tracks which (numId, ilvl) combinations have already had their
    /// startOverride applied. startOverride is only applied on first encounter.
    start_override_applied: HashMap<(u32, u32), bool>,
    /// Tracks the last ilvl seen for each numId, used for lvlRestart logic.
    last_ilvl: HashMap<u32, u32>,
}

impl NumberingState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Synthesize the number text for a paragraph with the given numPr.
    ///
    /// Returns the synthesized text (e.g., "1.", "(a)") or empty string for bullets.
    /// Updates internal counter state.
    pub fn synthesize(
        &mut self,
        definitions: &NumberingDefinitions,
        num_id: u32,
        ilvl: u32,
    ) -> Result<String, NumberingError> {
        let level = definitions.get_level(num_id, ilvl).ok_or_else(|| {
            // Try to give a more specific error
            match definitions.num_instances.get(&num_id) {
                Some(num_instance) => NumberingError::MissingLevel {
                    abstract_num_id: num_instance.abstract_num_id,
                    ilvl,
                },
                None => NumberingError::MissingAbstractNum { num_id },
            }
        })?;

        // Bullet format → return bullet character
        if level.num_fmt == NumFormat::Bullet {
            // lvlText contains the bullet character. Word often uses PUA chars
            // from Symbol font (e.g., U+F0B7 → bullet). Map those to "•".
            let ch = level.lvl_text.chars().next();
            return Ok(match ch {
                None => "•".to_string(),
                Some(c) if (c as u32) >= 0xF000 => "•".to_string(),
                Some(_) => level.lvl_text.clone(),
            });
        }
        // "none" and other non-numeric formats produce no text
        if !level.num_fmt.is_numeric() {
            return Ok(String::new());
        }

        // lvlRestart logic: determine which levels should be reset based on the
        // current level being hit. Per §17.9.10:
        // - restart_level = None (default): restart when the immediately previous level is encountered
        // - restart_level = Some(0): never restart
        // - restart_level = Some(n): restart when level n-1 (0-indexed) is encountered
        let prev_ilvl = self.last_ilvl.get(&num_id).copied();
        if let Some(prev) = prev_ilvl
            && ilvl <= prev
        {
            // A shallower-or-equal level was hit. Check deeper levels for restart.
            let keys: Vec<(u32, u32)> = self
                .counters
                .keys()
                .filter(|&&(nid, _)| nid == num_id)
                .copied()
                .collect();
            for (_, deeper_ilvl) in keys {
                if deeper_ilvl <= ilvl {
                    continue;
                }
                // Check this deeper level's restart_level
                let should_reset =
                    if let Some(deeper_level) = definitions.get_level(num_id, deeper_ilvl) {
                        match deeper_level.restart_level {
                            Some(0) => false, // never restart
                            Some(n) => {
                                // Restart when level n-1 (0-indexed) or any earlier level is used.
                                // The current ilvl being hit triggers restart if ilvl <= n-1.
                                ilvl < n
                            }
                            None => {
                                // Default: restart when the immediately previous level
                                // (deeper_ilvl - 1) or any earlier level is encountered.
                                ilvl < deeper_ilvl
                            }
                        }
                    } else {
                        // No level def found; use default behavior
                        ilvl < deeper_ilvl
                    };
                if should_reset {
                    self.counters.remove(&(num_id, deeper_ilvl));
                }
            }
        }

        self.last_ilvl.insert(num_id, ilvl);

        // Apply startOverride on first encounter of this (numId, ilvl) combination.
        // Per §17.9.26, startOverride resets the counter to a specific value.
        let start_override = definitions.get_start_override(num_id, ilvl);
        if let Some(override_val) = start_override
            && let std::collections::hash_map::Entry::Vacant(e) =
                self.start_override_applied.entry((num_id, ilvl))
        {
            e.insert(true);
            self.counters
                .insert((num_id, ilvl), i64::from(override_val) - 1);
        }

        // Get or initialize the counter for this level.
        // We store (start - 1) so that the first increment produces `start`.
        // Uses i64 because start can be 0 (spec default per §17.9.25).
        // effective_start applies MS-OI §2.1.292 (a `start` inside an override lvl
        // is ignored; use the abstract level's start) rather than level.start.
        let counter = self
            .counters
            .entry((num_id, ilvl))
            .or_insert(i64::from(definitions.effective_start(num_id, ilvl)) - 1);
        *counter += 1;
        let current_value = (*counter).max(0) as u32;

        // Format the number text using lvlText pattern
        let formatted = format_lvl_text(
            &level.lvl_text,
            ilvl,
            current_value,
            &level.num_fmt,
            level.is_legal,
            self,
            definitions,
            num_id,
        );

        Ok(formatted)
    }

    /// Get the current counter value for a specific level (used for multi-level patterns).
    fn get_counter(&self, num_id: u32, ilvl: u32) -> u32 {
        self.counters
            .get(&(num_id, ilvl))
            .copied()
            .unwrap_or(0)
            .max(0) as u32
    }
}

/// Format a number value according to the numFmt.
fn format_number(value: u32, fmt: &NumFormat) -> String {
    match fmt {
        NumFormat::Decimal => value.to_string(),
        NumFormat::LowerLetter => to_letter(value, false),
        NumFormat::UpperLetter => to_letter(value, true),
        NumFormat::LowerRoman => to_roman(value, false),
        NumFormat::UpperRoman => to_roman(value, true),
        NumFormat::Bullet | NumFormat::None | NumFormat::Unknown(_) => String::new(),
    }
}

/// Convert a number to letter notation (a, b, c, ..., z, aa, ab, ...).
fn to_letter(value: u32, uppercase: bool) -> String {
    if value == 0 {
        return String::new();
    }

    let mut result = String::new();
    let mut n = value;

    while n > 0 {
        n -= 1;
        let ch = ((n % 26) as u8 + if uppercase { b'A' } else { b'a' }) as char;
        result.insert(0, ch);
        n /= 26;
    }

    result
}

/// Convert a number to Roman numerals.
fn to_roman(value: u32, uppercase: bool) -> String {
    if value == 0 {
        return String::new();
    }

    let numerals = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];

    let mut result = String::new();
    let mut n = value;

    for &(val, numeral) in &numerals {
        while n >= val {
            result.push_str(numeral);
            n -= val;
        }
    }

    if uppercase {
        result.to_uppercase()
    } else {
        result
    }
}

/// Format lvlText pattern by replacing %1, %2, etc. with actual values.
///
/// lvlText patterns like "%1." or "(%2)" contain placeholders where:
/// - %1 = level 0 value
/// - %2 = level 1 value
/// - etc.
///
/// When `is_legal` is true (MS-OI29500 §17.9.4a), ALL %N references are
/// formatted as decimal, including the current level. The only exception is
/// levels with numFmt=none, which are preserved as empty (§17.9.4b).
///
/// If any %N reference points to a level higher than the current level
/// (MS-OI29500 §17.9.11c), the ENTIRE lvlText is ignored (returns empty).
#[allow(clippy::too_many_arguments)]
fn format_lvl_text(
    lvl_text: &str,
    current_ilvl: u32,
    current_value: u32,
    current_fmt: &NumFormat,
    is_legal: bool,
    state: &NumberingState,
    definitions: &NumberingDefinitions,
    num_id: u32,
) -> String {
    // MS-OI29500 §17.9.11c: If lvlText references any %N where N > current level + 1
    // (i.e., the 0-indexed level >= current_ilvl + 1), the entire lvlText is ignored.
    for n in 1..=9u32 {
        let placeholder = format!("%{n}");
        if lvl_text.contains(&placeholder) && (n - 1) > current_ilvl {
            return String::new();
        }
    }

    let mut result = lvl_text.to_string();

    // Replace %N placeholders (N is 1-indexed, ilvl is 0-indexed)
    for level in 0..=current_ilvl {
        let placeholder = format!("%{}", level + 1);
        if result.contains(&placeholder) {
            let value = if level == current_ilvl {
                current_value
            } else {
                state.get_counter(num_id, level)
            };

            // Get the format for this level.
            // When is_legal is set, ALL levels are forced to Decimal (MS-OI29500 §17.9.4a),
            // except levels with numFmt=none which are preserved (§17.9.4b).
            let level_fmt = if level == current_ilvl {
                current_fmt
            } else {
                definitions
                    .get_level(num_id, level)
                    .map(|l| &l.num_fmt)
                    .unwrap_or(current_fmt)
            };

            let fmt = if is_legal && *level_fmt != NumFormat::None {
                &NumFormat::Decimal
            } else {
                level_fmt
            };

            let formatted_value = format_number(value, fmt);
            result = result.replace(&placeholder, &formatted_value);
        }
    }

    result
}

fn parse_abstract_num(element: &Element) -> Option<AbstractNum> {
    // §17.9.1: abstractNumId is required. A w:abstractNum missing it cannot
    // be referenced by any w:num instance, so the whole definition is inert.
    // This is malformed OOXML a compliant producer never emits, but we don't
    // want one garbled abstractNum in numbering.xml to refuse the whole
    // document — skip it and say so (parse totality, invariant #1).
    let abstract_num_id = match attr_value_u32(element, "abstractNumId") {
        Some(id) => id,
        None => {
            tracing::warn!(
                "w:abstractNum missing required abstractNumId attribute; skipping definition"
            );
            return None;
        }
    };
    let mut levels = HashMap::new();
    let mut num_style_link = None;
    let mut style_link = None;

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "lvl") {
            if let Some(level) = parse_level(el) {
                levels.insert(level.ilvl, level);
            }
        } else if is_w_tag(el, "numStyleLink") {
            // §17.9.21: references a numbering style for the actual levels
            num_style_link = attr_value(el, "val").map(|s| s.to_string());
        } else if is_w_tag(el, "styleLink") {
            // §17.9.27: declares this abstractNum as the source for a style
            style_link = attr_value(el, "val").map(|s| s.to_string());
        }
    }

    Some(AbstractNum {
        abstract_num_id,
        levels,
        num_style_link,
        style_link,
    })
}

fn parse_level(element: &Element) -> Option<LevelDef> {
    // §17.9.6: ilvl is required. Without it we can't key the level, so this
    // one <w:lvl> is skipped (isolated to the single level, not the whole
    // abstractNum) with an observable warning rather than silently vanishing.
    let ilvl = match attr_value_u32(element, "ilvl") {
        Some(v) => v,
        None => {
            tracing::warn!("w:lvl missing required ilvl attribute; skipping level definition");
            return None;
        }
    };
    let mut num_fmt = NumFormat::Decimal;
    // ISO 29500-1 §17.9.25: "If this element is omitted, then the starting
    // value shall be zero (0)."
    let mut start = 0u32;
    let mut lvl_text = String::new();
    let mut is_legal = false;
    let mut restart_level = None;
    let mut indent = None;
    let mut p_style = None;
    let mut suffix = LevelSuffix::default();

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "numFmt") {
            if let Some(val) = attr_value(el, "val") {
                num_fmt = NumFormat::from_str(val);
            }
        } else if is_w_tag(el, "start") {
            // The §17.9.25 zero-default above is for the OMITTED element.
            // A *present* but unparseable w:val is different: it's malformed
            // producer output, not an intentional "start at 0". Coercing both
            // to the same value silently would hide the distinction — warn
            // and fall back to the spec default instead.
            if let Some(val) = attr_value(el, "val") {
                start = val.parse().unwrap_or_else(|e| {
                    tracing::warn!(
                        ilvl,
                        value = %val,
                        error = %e,
                        "w:start has an unparseable value; defaulting to 0"
                    );
                    0
                });
            }
        } else if is_w_tag(el, "lvlText")
            && let Some(val) = attr_value(el, "val")
        {
            lvl_text = val.to_string();
        } else if is_w_tag(el, "isLgl") {
            // §17.9.4: presence of element means true (common boolean property).
            // If val is present, "0" or "false" means off.
            is_legal = !matches!(attr_value(el, "val"), Some(v) if v == "0" || v == "false");
        } else if is_w_tag(el, "lvlRestart") {
            // §17.9.10: val is a 1-based index; 0 means never restart.
            restart_level = attr_value_u32(el, "val");
        } else if is_w_tag(el, "pPr") {
            // §17.9.22: numbering level paragraph properties (indentation).
            indent = parse_level_indent(el);
        } else if is_w_tag(el, "pStyle") {
            // §17.9.23: reverse binding — this level claims a paragraph style.
            p_style = attr_value(el, "val").map(|s| s.to_string());
        } else if is_w_tag(el, "suff") {
            // §17.9.28: separator between numbering text and paragraph body.
            if let Some(val) = attr_value(el, "val") {
                suffix = LevelSuffix::from_str(val);
            }
        }
    }

    Some(LevelDef {
        ilvl,
        num_fmt,
        start,
        lvl_text,
        is_legal,
        restart_level,
        indent,
        p_style,
        suffix,
    })
}

/// Parse an `i32` numbering-level indent attribute, warning (rather than
/// silently coalescing to "absent") when the attribute is present but its
/// value doesn't parse — a malformed value is not the same as an omitted one.
fn parse_indent_i32(v: &str, attr_name: &str) -> Option<i32> {
    match v.parse() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(
                attr = attr_name,
                value = %v,
                error = %e,
                "numbering level w:ind attribute has an unparseable value; ignoring"
            );
            None
        }
    }
}

/// Parse `w:ind` from a numbering level's `w:pPr` element (§17.9.22).
fn parse_level_indent(p_pr: &Element) -> Option<LevelIndent> {
    let ind = p_pr.children.iter().find_map(|child| {
        if let XMLNode::Element(el) = child
            && is_w_tag(el, "ind")
        {
            return Some(el);
        }
        None
    })?;

    let left = attr_value(ind, "left")
        .or_else(|| attr_value(ind, "start"))
        .and_then(|v| parse_indent_i32(v, "left/start"));

    let right = attr_value(ind, "right")
        .or_else(|| attr_value(ind, "end"))
        .and_then(|v| parse_indent_i32(v, "right/end"));

    // §17.3.1.12: "firstLine and hanging are mutually exclusive,
    // if both are specified, the firstLine value is ignored" — hanging wins.
    let first_line = if let Some(hanging) = attr_value(ind, "hanging") {
        parse_indent_i32(hanging, "hanging").map(|v| -v)
    } else if let Some(first) = attr_value(ind, "firstLine") {
        parse_indent_i32(first, "firstLine")
    } else {
        None
    };

    if left.is_some() || right.is_some() || first_line.is_some() {
        Some(LevelIndent {
            left,
            right,
            effective_first_line_twips: first_line,
        })
    } else {
        None
    }
}

fn parse_num_instance(element: &Element) -> Option<NumInstance> {
    // §17.9.18: numId is required to key this instance; without it the
    // definition can never be referenced by a paragraph's numPr.
    let num_id = match attr_value_u32(element, "numId") {
        Some(id) => id,
        None => {
            tracing::warn!("w:num missing required numId attribute; skipping numbering instance");
            return None;
        }
    };
    let mut abstract_num_id = None;
    let mut level_overrides = HashMap::new();

    for child in &element.children {
        let el = match child {
            XMLNode::Element(el) => el,
            _ => continue,
        };

        if is_w_tag(el, "abstractNumId") {
            abstract_num_id = attr_value_u32(el, "val");
        } else if is_w_tag(el, "lvlOverride")
            && let Some(override_ilvl) = attr_value_u32(el, "ilvl")
        {
            let mut level = None;
            let mut start_override = None;

            for ovr_child in &el.children {
                let ovr_el = match ovr_child {
                    XMLNode::Element(e) => e,
                    _ => continue,
                };

                if is_w_tag(ovr_el, "lvl") {
                    level = parse_level(ovr_el);
                } else if is_w_tag(ovr_el, "startOverride") {
                    start_override = attr_value_u32(ovr_el, "val");
                }
            }

            level_overrides.insert(
                override_ilvl,
                LevelOverride {
                    level,
                    start_override,
                },
            );
        }
    }

    // §17.9.2: abstractNumId is a required child linking this instance to its
    // definition. Without it the instance is unusable — skip it, but keep
    // importing the rest of numbering.xml (isolated to this one w:num).
    let Some(abstract_num_id) = abstract_num_id else {
        tracing::warn!(
            num_id,
            "w:num missing required abstractNumId child; skipping numbering instance"
        );
        return None;
    };

    Some(NumInstance {
        num_id,
        abstract_num_id,
        level_overrides,
    })
}

fn is_w_tag(element: &Element, local: &str) -> bool {
    if element.name == local {
        if element.prefix.as_deref() == Some("w") {
            return true;
        }
        return element.namespace.as_deref() == Some(WORD_NS);
    }
    element.name == format!("w:{local}")
}

fn attr_value<'a>(element: &'a Element, local: &str) -> Option<&'a String> {
    attr_get(element, local)
}

fn attr_value_u32(element: &Element, local: &str) -> Option<u32> {
    attr_value(element, local).and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_letter_lowercase() {
        assert_eq!(to_letter(1, false), "a");
        assert_eq!(to_letter(2, false), "b");
        assert_eq!(to_letter(26, false), "z");
        assert_eq!(to_letter(27, false), "aa");
        assert_eq!(to_letter(28, false), "ab");
        assert_eq!(to_letter(52, false), "az");
        assert_eq!(to_letter(53, false), "ba");
    }

    #[test]
    fn test_to_letter_uppercase() {
        assert_eq!(to_letter(1, true), "A");
        assert_eq!(to_letter(26, true), "Z");
        assert_eq!(to_letter(27, true), "AA");
    }

    #[test]
    fn test_to_roman_lowercase() {
        assert_eq!(to_roman(1, false), "i");
        assert_eq!(to_roman(2, false), "ii");
        assert_eq!(to_roman(3, false), "iii");
        assert_eq!(to_roman(4, false), "iv");
        assert_eq!(to_roman(5, false), "v");
        assert_eq!(to_roman(9, false), "ix");
        assert_eq!(to_roman(10, false), "x");
        assert_eq!(to_roman(14, false), "xiv");
        assert_eq!(to_roman(50, false), "l");
        assert_eq!(to_roman(100, false), "c");
        assert_eq!(to_roman(500, false), "d");
        assert_eq!(to_roman(1000, false), "m");
    }

    #[test]
    fn test_to_roman_uppercase() {
        assert_eq!(to_roman(1, true), "I");
        assert_eq!(to_roman(4, true), "IV");
        assert_eq!(to_roman(9, true), "IX");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1, &NumFormat::Decimal), "1");
        assert_eq!(format_number(10, &NumFormat::Decimal), "10");
        assert_eq!(format_number(1, &NumFormat::LowerLetter), "a");
        assert_eq!(format_number(1, &NumFormat::UpperLetter), "A");
        assert_eq!(format_number(4, &NumFormat::LowerRoman), "iv");
        assert_eq!(format_number(4, &NumFormat::UpperRoman), "IV");
        assert_eq!(format_number(1, &NumFormat::Bullet), "");
    }

    #[test]
    fn test_parse_numbering_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="(%2)"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        assert_eq!(defs.abstract_nums.len(), 1);
        assert_eq!(defs.num_instances.len(), 1);

        let level0 = defs.get_level(1, 0).unwrap();
        assert_eq!(level0.num_fmt, NumFormat::Decimal);
        assert_eq!(level0.lvl_text, "%1.");
        assert_eq!(level0.start, 1);

        let level1 = defs.get_level(1, 1).unwrap();
        assert_eq!(level1.num_fmt, NumFormat::LowerLetter);
        assert_eq!(level1.lvl_text, "(%2)");
    }

    #[test]
    fn test_synthesize_decimal() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "1.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "2.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "3.");
    }

    #[test]
    fn test_synthesize_letter() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="(%2)"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "1.");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "(a)");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "(b)");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "2.");
        // Level 1 should reset after level 0 increments
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "(a)");
    }

    #[test]
    fn test_synthesize_bullet_returns_bullet_char() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="bullet"/>
      <w:lvlText w:val=""/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Empty lvlText → defaults to "•"
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "•");
    }

    #[test]
    fn test_synthesize_bullet_pua_char_maps_to_bullet() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="bullet"/>
      <w:lvlText w:val="&#xF0B7;"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // PUA character (U+F0B7) should map to "•"
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "•");
    }

    #[test]
    fn test_synthesize_bullet_regular_char_preserved() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="bullet"/>
      <w:lvlText w:val="-"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Regular character preserved as-is
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "-");
    }

    #[test]
    fn test_synthesize_with_start_value() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="5"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "5.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "6.");
    }

    // ---------------------------------------------------------------
    // lvlOverride + startOverride tests
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_lvl_override_with_start_override() {
        // Two num instances share the same abstract definition.
        // numId=2 has a lvlOverride with startOverride that restarts level 0 at 1.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:startOverride w:val="1"/>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();

        // numId=1 has no overrides
        let inst1 = defs.num_instances.get(&1).unwrap();
        assert!(inst1.level_overrides.is_empty());

        // numId=2 has a startOverride on level 0
        let inst2 = defs.num_instances.get(&2).unwrap();
        let ovr = inst2.level_overrides.get(&0).unwrap();
        assert!(ovr.level.is_none());
        assert_eq!(ovr.start_override, Some(1));
    }

    #[test]
    fn test_parse_lvl_override_with_full_level() {
        // lvlOverride contains a full w:lvl replacement
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:lvl w:ilvl="0">
        <w:start w:val="4"/>
        <w:numFmt w:val="upperLetter"/>
        <w:lvlText w:val="%1)"/>
      </w:lvl>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();

        // get_level should return the override level, not the abstract one
        let level = defs.get_level(1, 0).unwrap();
        assert_eq!(level.num_fmt, NumFormat::UpperLetter);
        assert_eq!(level.lvl_text, "%1)");
        assert_eq!(level.start, 4);
    }

    #[test]
    fn test_synthesize_lvl_override_full_replacement() {
        // Abstract defines decimal, but the override changes to upper letter starting at 4
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:lvl w:ilvl="0">
        <w:start w:val="1"/>
        <w:numFmt w:val="upperLetter"/>
        <w:lvlText w:val="%1)"/>
      </w:lvl>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Should use the override format (upper letter) instead of abstract (decimal)
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "A)");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "B)");
    }

    #[test]
    fn test_synthesize_start_override_restarts_numbering() {
        // Simulates a legal document pattern: Article 1 uses numId=1, Article 2
        // uses numId=2 which has a startOverride to restart at 1.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:startOverride w:val="1"/>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Article 1 section: numId=1, counts 1, 2, 3
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "1.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "2.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "3.");

        // Article 2 section: numId=2 with startOverride=1, restarts to 1
        assert_eq!(state.synthesize(&defs, 2, 0).unwrap(), "1.");
        assert_eq!(state.synthesize(&defs, 2, 0).unwrap(), "2.");
    }

    #[test]
    fn test_start_override_applied_only_once() {
        // startOverride should only apply on FIRST encounter, not every time.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="100"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:startOverride w:val="5"/>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // First encounter: startOverride=5 takes effect
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "5.");
        // Subsequent: counter continues from 5, does NOT reset to 5 again
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "6.");
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "7.");
    }

    // ---------------------------------------------------------------
    // isLgl (legal numbering) tests
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_is_legal() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="upperLetter"/>
      <w:lvlText w:val="%1"/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="%1.%2"/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:isLgl/>
      <w:lvlText w:val="%1.%2.%3"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();

        let level0 = defs.get_level(1, 0).unwrap();
        assert!(!level0.is_legal);

        let level2 = defs.get_level(1, 2).unwrap();
        assert!(level2.is_legal);
    }

    #[test]
    fn test_is_legal_false_with_val_zero() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:isLgl w:val="0"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let level = defs.get_level(1, 0).unwrap();
        assert!(!level.is_legal);
    }

    #[test]
    fn test_synthesize_legal_numbering() {
        // Legal outline numbering: levels 0 and 1 use letters, but level 2
        // has isLgl, so %1 and %2 references are forced to decimal.
        // This mirrors the spec example from §17.9.4:
        //   Level 0: A, B, ... (upperLetter)
        //   Level 1: a, b, ... (lowerLetter)
        //   Level 2: isLgl → "1.2.1" instead of "A.b.1"
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="upperLetter"/>
      <w:lvlText w:val="%1"/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="%1.%2"/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:isLgl/>
      <w:lvlText w:val="%1.%2.%3"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Level 0: "A" (upperLetter, no isLgl)
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "A");
        // Level 1: "A.a" (no isLgl, uses each level's own format)
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "A.a");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "A.b");
        // Level 2 with isLgl: forces %1 and %2 to decimal → "1.2.1"
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.2.1");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.2.2");

        // Advance level 0
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "B");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "B.a");
        // isLgl on level 2 again → "2.1.1"
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "2.1.1");
    }

    // ---------------------------------------------------------------
    // lvlRestart tests
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_lvl_restart() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1)"/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="upperLetter"/>
      <w:lvlText w:val="%2)"/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerRoman"/>
      <w:lvlRestart w:val="0"/>
      <w:lvlText w:val="%3)"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();

        let level0 = defs.get_level(1, 0).unwrap();
        assert_eq!(level0.restart_level, None);

        let level2 = defs.get_level(1, 2).unwrap();
        assert_eq!(level2.restart_level, Some(0));
    }

    #[test]
    fn test_lvl_restart_zero_never_restarts() {
        // §17.9.10 spec example: lvlRestart=0 means "never restart".
        // Level 0: 1), 2)
        // Level 1: A), B) (default restart → resets after level 0)
        // Level 2: i), ii), iii), iv) (lvlRestart=0 → never resets)
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1)"/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="upperLetter"/>
      <w:lvlText w:val="%2)"/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerRoman"/>
      <w:lvlRestart w:val="0"/>
      <w:lvlText w:val="%3)"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Following the spec example from §17.9.10:
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "1)"); // 1)
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "A)"); // A)
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "i)"); // i)
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "ii)"); // ii)

        // Level 0 hits again → level 1 should restart, level 2 should NOT
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "2)"); // 2)
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "A)"); // A) (restarted)
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "iii)"); // iii) (not restarted!)
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "iv)"); // iv)
    }

    #[test]
    fn test_lvl_restart_custom_level() {
        // Level 2 has lvlRestart=2, meaning it restarts only when level 1 (index 1)
        // or an earlier level is encountered. So encountering level 0 resets it,
        // and encountering level 1 resets it, but it persists across consecutive uses.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1.%2."/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlRestart w:val="2"/>
      <w:lvlText w:val="%1.%2.%3."/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Build up: 1. -> 1.1. -> 1.1.1. -> 1.1.2.
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "1.");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "1.1.");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.1.1.");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.1.2.");

        // Hit level 1 again → level 2 should restart (lvlRestart=2, so level 1 triggers it)
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "1.2.");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.2.1."); // restarted

        // Hit level 0 → both level 1 and level 2 restart
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "2.");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "2.1.");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "2.1.1."); // restarted
    }

    // ---------------------------------------------------------------
    // Combined / integration tests
    // ---------------------------------------------------------------

    #[test]
    fn test_legal_contract_with_overrides_and_legal_numbering() {
        // Simulates a multi-section legal contract:
        // - Abstract defines a 3-level outline (Article / Section / Clause)
        // - Level 2 uses isLgl for "1.1.1" style
        // - numId=2 has a startOverride on level 0 to restart at 1 for Article 2
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="upperRoman"/>
      <w:lvlText w:val="Article %1."/>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="%2)"/>
    </w:lvl>
    <w:lvl w:ilvl="2">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:isLgl/>
      <w:lvlText w:val="%1.%2.%3"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0">
      <w:startOverride w:val="10"/>
    </w:lvlOverride>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let mut state = NumberingState::new();

        // Article I
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "Article I.");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "a)");
        // isLgl on level 2 forces %1 (upperRoman->decimal) and %2 (lowerLetter->decimal)
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "1.1.1");

        // Article II
        assert_eq!(state.synthesize(&defs, 1, 0).unwrap(), "Article II.");
        assert_eq!(state.synthesize(&defs, 1, 1).unwrap(), "a)");
        assert_eq!(state.synthesize(&defs, 1, 2).unwrap(), "2.1.1");

        // Switch to numId=2 which has startOverride=10 on level 0
        assert_eq!(state.synthesize(&defs, 2, 0).unwrap(), "Article X.");
    }

    /// A single malformed `w:abstractNum` (missing the required
    /// `abstractNumId`) must not take down the rest of numbering.xml — the
    /// other, well-formed definitions still load. This is the parse-totality
    /// boundary: `NumberingDefinitions::parse` degrades per-definition, not
    /// per-document.
    #[test]
    fn malformed_abstract_num_is_skipped_others_still_load() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum>
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
  </w:abstractNum>
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        assert_eq!(
            defs.abstract_nums.len(),
            1,
            "the abstractNumId-less definition is dropped, the valid one keeps"
        );
        assert!(defs.get_level(1, 0).is_some(), "numId=1 still resolves");
    }

    /// A `w:lvl` missing the required `ilvl` is skipped in isolation — the
    /// abstractNum's other, well-formed levels still load.
    #[test]
    fn malformed_level_is_skipped_others_still_load() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
    <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="(%2)"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        assert!(defs.get_level(1, 0).is_none(), "ilvl-less lvl is dropped");
        assert!(defs.get_level(1, 1).is_some(), "level 1 still resolves");
    }

    /// A `w:num` missing the required `abstractNumId` child is skipped —
    /// other, well-formed instances still load.
    #[test]
    fn malformed_num_instance_is_skipped_others_still_load() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"/>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        assert_eq!(
            defs.num_instances.len(),
            1,
            "numId=1 has no abstractNumId child, dropped"
        );
        assert!(defs.get_level(2, 0).is_some(), "numId=2 still resolves");
    }

    /// §17.9.25's zero default is for an OMITTED `w:start`. A *present* but
    /// unparseable value ("one" instead of "1") must still default to zero
    /// (isolated degradation, not a hard parse failure) — this pins that
    /// behavior distinctly from the omitted case.
    #[test]
    fn malformed_start_value_defaults_to_zero() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="one"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        assert_eq!(defs.get_level(1, 0).unwrap().start, 0);
    }

    /// A present-but-malformed `w:ind` attribute is ignored (treated as
    /// absent) rather than failing the whole level definition.
    #[test]
    fn malformed_indent_attr_is_ignored() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>
      <w:pPr><w:ind w:left="not-a-number" w:right="360"/></w:pPr>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1">
    <w:abstractNumId w:val="0"/>
  </w:num>
</w:numbering>"#;

        let defs = NumberingDefinitions::parse(xml.as_bytes()).unwrap();
        let indent = defs.get_level(1, 0).unwrap().indent.as_ref().unwrap();
        assert_eq!(
            indent.left, None,
            "malformed left is ignored, not defaulted to 0"
        );
        assert_eq!(indent.right, Some(360));
    }
}
