//! The authoring paths' change plan keeps the shared default heuristics.
//!
//! A document comparison passes its own configuration
//! (`diff_block_content_resolving_opaques_with_config`), so an explicit
//! edit's plan is what it was: a long sentence rewritten around a few kept
//! words is still one deletion and one insertion (the bail-out), and the
//! default entry point is exactly the configured one with the default
//! heuristics.

use std::collections::HashMap;

use stemma::domain::{InlineChange, InlineNode, ParagraphNode};
use stemma::local_change::{
    DiffHeuristics, diff_block_content_resolving_opaques,
    diff_block_content_resolving_opaques_with_config,
};

fn inlines(text: &str) -> Vec<InlineNode> {
    ParagraphNode::new_story_body("p", text, None).all_inlines_owned()
}

fn count(plan: &[InlineChange]) -> (usize, usize) {
    let deleted = plan
        .iter()
        .filter(|change| matches!(change, InlineChange::Deleted { .. }))
        .count();
    let inserted = plan
        .iter()
        .filter(|change| matches!(change, InlineChange::Inserted { .. }))
        .count();
    (deleted, inserted)
}

#[test]
fn authoring_plans_keep_the_default_heuristics() {
    let old = inlines("The obligations of the parties shall be governed by applicable law here.");
    let new = inlines("Each participant must comply with relevant statutory requirements here.");

    let default = diff_block_content_resolving_opaques(&old, &new, &HashMap::new());
    let configured = diff_block_content_resolving_opaques_with_config(
        &old,
        &new,
        &HashMap::new(),
        &DiffHeuristics::from_env(),
    );

    assert_eq!(default, configured);
    assert_eq!(
        count(&default),
        (1, 1),
        "one whole replacement: {default:?}"
    );

    let word_level = DiffHeuristics {
        bail_out_similarity_threshold: 0.0,
        zipper_min_change_runs: usize::MAX,
        ..DiffHeuristics::from_env()
    };
    let fine =
        diff_block_content_resolving_opaques_with_config(&old, &new, &HashMap::new(), &word_level);
    assert!(
        count(&fine).0 > 1,
        "the comparison's configuration is distinct from the default: {fine:?}"
    );
}
