//! A whole-row permutation is one authoring intent, not a sequence of settled moves.

use stemma::edit_v4::TableOpWire;

#[test]
fn complete_row_permutation_is_an_explicit_wire_operation() {
    let request = r#"{"kind":"reorder_rows","row_order":[2,0,3,1]}"#;
    assert!(
        serde_json::from_str::<TableOpWire>(request).is_ok(),
        "One operation must express a complete source-row permutation"
    );
}

#[test]
fn row_permutation_wire_does_not_guess_missing_or_invalid_indices() {
    for request in [
        r#"{"kind":"reorder_rows"}"#,
        r#"{"kind":"reorder_rows","row_order":null}"#,
        r#"{"kind":"reorder_rows","row_order":[-1,0]}"#,
        r#"{"kind":"reorder_rows","row_order":[0.5,1]}"#,
        r#"{"kind":"reorder_rows","row_order":["0",1]}"#,
        r#"{"kind":"reorder_rows","row_order":[1,0],"cells":["replacement"]}"#,
    ] {
        assert!(
            serde_json::from_str::<TableOpWire>(request).is_err(),
            "{request}"
        );
    }
}
