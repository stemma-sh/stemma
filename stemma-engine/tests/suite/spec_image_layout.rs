//! Wire-edge schema/adapter spec for `set_image_layout` (crop / position / wrap).
//!
//! These pin the JSON edge so a malformed request fails *before* it reaches the
//! engine (CLAUDE.md "parse at the edges, fail fast"):
//!   - an empty request (no crop/position/wrap) is refused;
//!   - a position axis must set exactly one of `offset` / `align`;
//!   - an unknown `wrap` token is refused (never defaulted);
//!   - a crop edge inset outside `0..=100000` (1000ths of a percent) is refused;
//!   - valid requests carry their values through translation onto the domain
//!     `ImageLayoutPatch`.
//!
//! Daily tier, corpus-free.

use stemma::edit::{EditStep, ImageCrop, ImagePositionAxis, ImageWrapType};
use stemma::edit_v4::{SchemaError, parse_transaction};

fn op(extra: &str) -> String {
    format!(
        r#"{{
          "ops": [{{ "op": "set_image_layout", "target": "p_1", "drawing_id": "p_1_widget_0"{extra} }}],
          "revision": {{ "author": "wire" }}
        }}"#
    )
}

fn step(extra: &str) -> EditStep {
    parse_transaction(&op(extra))
        .expect("schema accepts")
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0)
}

#[test]
fn empty_request_is_rejected_at_the_wire() {
    let err = parse_transaction(&op("")).expect_err("reject");
    assert!(
        matches!(err, SchemaError::EmptyImageLayout { .. }),
        "got {err:?}"
    );
}

#[test]
fn crop_only_with_all_none_edges_is_rejected() {
    let err = parse_transaction(&op(r#", "crop": {}"#)).expect_err("reject");
    assert!(
        matches!(err, SchemaError::EmptyImageLayout { .. }),
        "got {err:?}"
    );
}

#[test]
fn position_with_both_offset_and_align_is_rejected() {
    let err = parse_transaction(&op(
        r#", "position_h": { "relative_from": "page", "offset": 10, "align": "left" }"#,
    ))
    .expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::ImageLayoutPositionAmbiguous {
                axis: "position_h",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn position_with_neither_offset_nor_align_is_rejected() {
    let err = parse_transaction(&op(r#", "position_v": { "relative_from": "margin" }"#))
        .expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::ImageLayoutPositionAmbiguous {
                axis: "position_v",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn unknown_wrap_token_is_rejected() {
    let err = parse_transaction(&op(r#", "wrap": "diagonal""#)).expect_err("reject");
    assert!(
        matches!(err, SchemaError::ImageLayoutUnknownWrap { ref token, .. } if token == "diagonal"),
        "got {err:?}"
    );
}

#[test]
fn crop_inset_above_100_percent_is_rejected() {
    let err = parse_transaction(&op(r#", "crop": { "left": 100001 }"#)).expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::ImageLayoutCropOutOfRange {
                edge: "left",
                value: 100001,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn negative_crop_inset_is_rejected() {
    let err = parse_transaction(&op(r#", "crop": { "bottom": -1 }"#)).expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::ImageLayoutCropOutOfRange {
                edge: "bottom",
                value: -1,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn crop_boundaries_zero_and_full_are_accepted() {
    // 0 and 100000 are the valid ST_Percentage boundaries.
    match step(r#", "crop": { "left": 0, "right": 100000 }"#) {
        EditStep::SetImageLayout { patch, .. } => {
            let crop = patch.crop.expect("crop present");
            assert_eq!(crop.left, Some(0));
            assert_eq!(crop.right, Some(100_000));
            assert_eq!(crop.top, None, "omitted edges stay None");
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn wrap_tokens_map_to_domain_variants() {
    for (token, expected) in [
        ("none", ImageWrapType::None),
        ("square", ImageWrapType::Square),
        ("tight", ImageWrapType::Tight),
        ("through", ImageWrapType::Through),
        ("top_and_bottom", ImageWrapType::TopAndBottom),
    ] {
        match step(&format!(r#", "wrap": "{token}""#)) {
            EditStep::SetImageLayout { patch, .. } => {
                assert_eq!(patch.wrap, Some(expected), "token {token}")
            }
            other => panic!("got {other:?}"),
        }
    }
}

#[test]
fn position_offset_and_align_carry_through_translation() {
    match step(r#", "position_h": { "relative_from": "page", "offset": -914400 }"#) {
        EditStep::SetImageLayout { patch, .. } => {
            assert_eq!(
                patch.position_h,
                Some(ImagePositionAxis::Offset {
                    relative_from: "page".to_string(),
                    offset_emu: -914_400, // negative offsets are valid
                })
            );
        }
        other => panic!("got {other:?}"),
    }
    match step(r#", "position_v": { "relative_from": "margin", "align": "center" }"#) {
        EditStep::SetImageLayout { patch, .. } => {
            assert_eq!(
                patch.position_v,
                Some(ImagePositionAxis::Align {
                    relative_from: "margin".to_string(),
                    align: "center".to_string(),
                })
            );
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn full_patch_round_trips_to_domain() {
    let s = step(
        r#", "crop": { "left": 5000, "top": 6000, "right": 7000, "bottom": 8000 },
            "position_h": { "relative_from": "column", "offset": 100 },
            "wrap": "square""#,
    );
    match s {
        EditStep::SetImageLayout { patch, .. } => {
            assert_eq!(
                patch.crop,
                Some(ImageCrop {
                    left: Some(5000),
                    top: Some(6000),
                    right: Some(7000),
                    bottom: Some(8000),
                })
            );
            assert!(patch.position_h.is_some());
            assert_eq!(patch.wrap, Some(ImageWrapType::Square));
        }
        other => panic!("got {other:?}"),
    }
}
