//! Validation shared by the wire edge and typed insertion callers.
use crate::word_ir::TabStopDef;

pub(crate) fn validate(stops: &[TabStopDef]) -> Result<(), String> {
    let mut positions = std::collections::BTreeSet::new();
    for stop in stops {
        // MS-OI29500 2.1.61: Word supports -22 through +22 inches.
        if !(-31680..=31680).contains(&stop.position) {
            return Err(format!(
                "tab position {} is outside Word's -31680..=31680 twip range",
                stop.position
            ));
        }
        if !positions.insert(stop.position) {
            return Err(format!(
                "duplicate tab position {}: supply one explicit stop or clear entry per position",
                stop.position
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TabAlignment;

    #[test]
    fn typed_callers_cannot_bypass_position_validation() {
        let stop = |position| TabStopDef {
            position,
            alignment: TabAlignment::Left,
            leader: None,
        };
        assert!(validate(&[]).is_ok());
        assert!(validate(&[stop(-31680), stop(31680)]).is_ok());
        assert!(validate(&[stop(-31681)]).is_err());
        assert!(validate(&[stop(31681)]).is_err());
        assert!(validate(&[stop(720), stop(720)]).is_err());
    }
}
