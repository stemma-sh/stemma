//! Wire-edge schema/adapter spec for `set_image_attrs` (Part B).
//!
//! The drawing `wp:extent` is a `ST_PositiveCoordinate` (§20.4.3.6): cx/cy are
//! non-negative integer EMUs. The wire adapter must (a) reject a negative cx/cy
//! at the schema layer (no silent clamp), (b) reject an empty request, (c)
//! preserve the alt-text three-state across `Option<Option<String>>`, and
//! (d) carry integer cx/cy through translation. These are pinned at the JSON
//! edge so a malformed request fails before it ever reaches the engine.
//!
//! Daily tier, corpus-free.

use stemma::edit::EditStep;
use stemma::edit_v4::{SchemaError, parse_transaction};

fn op(extra: &str) -> String {
    format!(
        r#"{{
          "ops": [{{ "op": "set_image_attrs", "target": "p_1", "drawing_id": "p_1_widget_0"{extra} }}],
          "revision": {{ "author": "wire" }}
        }}"#
    )
}

#[test]
fn negative_cx_is_rejected_at_the_wire() {
    let err = parse_transaction(&op(r#", "resize": { "cx": -5, "cy": 10 }"#)).expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::NegativeImageDimension {
                axis: "cx",
                value: -5,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn negative_cy_is_rejected_at_the_wire() {
    let err = parse_transaction(&op(r#", "resize": { "cx": 5, "cy": -10 }"#)).expect_err("reject");
    assert!(
        matches!(
            err,
            SchemaError::NegativeImageDimension {
                axis: "cy",
                value: -10,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn empty_request_is_rejected_at_the_wire() {
    let err = parse_transaction(&op("")).expect_err("reject");
    assert!(
        matches!(err, SchemaError::EmptyImageAttrs { .. }),
        "got {err:?}"
    );
}

#[test]
fn zero_dimensions_are_accepted() {
    // 0 is a valid ST_PositiveCoordinate boundary (non-negative), not an error.
    let txn = parse_transaction(&op(r#", "resize": { "cx": 0, "cy": 0 }"#)).expect("accept");
    let step = txn
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0);
    match step {
        EditStep::SetImageAttributes { resize, .. } => {
            let r = resize.expect("resize present");
            assert_eq!(r.cx_emu, 0);
            assert_eq!(r.cy_emu, 0);
        }
        other => panic!("expected SetImageAttributes, got {other:?}"),
    }
}

#[test]
fn integer_cx_cy_carry_through_translation() {
    let txn = parse_transaction(&op(r#", "resize": { "cx": 914400, "cy": 685800 }"#)).expect("ok");
    let step = txn
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0);
    match step {
        EditStep::SetImageAttributes { resize, .. } => {
            let r = resize.expect("resize present");
            assert_eq!(r.cx_emu, 914400);
            assert_eq!(r.cy_emu, 685800);
        }
        other => panic!("expected SetImageAttributes, got {other:?}"),
    }
}

#[test]
fn alt_text_three_state_survives_translation() {
    // present string -> Some(Some(s))
    let set = parse_transaction(&op(r#", "alt_text": "a logo""#))
        .expect("ok")
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0);
    match set {
        EditStep::SetImageAttributes { alt_text, .. } => {
            assert_eq!(alt_text, Some(Some("a logo".to_string())))
        }
        other => panic!("got {other:?}"),
    }

    // explicit null -> Some(None) (clear), distinct from omitted
    let clear = parse_transaction(&op(r#", "alt_text": null"#))
        .expect("ok")
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0);
    match clear {
        EditStep::SetImageAttributes { alt_text, .. } => assert_eq!(alt_text, Some(None)),
        other => panic!("got {other:?}"),
    }

    // omitted alt_text but a resize present -> None (leave)
    let leave = parse_transaction(&op(r#", "resize": { "cx": 1, "cy": 1 }"#))
        .expect("ok")
        .into_edit_transaction()
        .expect("translate")
        .steps
        .remove(0);
    match leave {
        EditStep::SetImageAttributes { alt_text, .. } => assert_eq!(alt_text, None),
        other => panic!("got {other:?}"),
    }
}
