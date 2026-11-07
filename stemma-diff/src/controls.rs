//! Block content controls in the merged comparison document.
//!
//! A content control (`w:sdt`, §17.5.2) has no revision, so the redline must
//! hold controls that both readings agree on: Reject All shows each source
//! block inside the source's controls, Accept All each target block inside
//! the target's. The compiler aligns two blocks only when their control
//! chains match (`separate_blocks_in_different_controls`), so a merged block
//! sits in its source document's controls if it came from the source and in
//! the target's if it was inserted. Source and target controls are the same
//! control when an aligned block pair joins them, or when a source-only and a
//! target-only control with the same key meet (the control's content was
//! replaced). The merged controls are rebuilt from that membership, each
//! with the target's properties when the target has the control.

use std::collections::HashMap;

use stemma::block_controls::{membership, rebuild, tracked_block_id};
use stemma::domain::{NodeId, SdtWrapper, TrackedBlock};

use crate::model::BlockProvenance;

/// What makes a source and a target control the same control: Word's control
/// id when its properties carry one (a control keeps it while, say, filling
/// it clears `w:showingPlcHdr`), else all of its properties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControlKey {
    Id(String),
    Properties(SdtWrapper),
}

pub(crate) fn control_key(wrapper: &SdtWrapper) -> ControlKey {
    match wrapper.control_id() {
        Some(id) => ControlKey::Id(id),
        None => ControlKey::Properties(wrapper.clone()),
    }
}

/// Rebuild the controls of the merged body `merged` from the source and
/// target bodies. `Err` names a control whose blocks would not stay together,
/// which no tracked change can express.
pub(crate) fn rebuild_merged_controls(
    merged: &mut [TrackedBlock],
    base: &[TrackedBlock],
    target: &[TrackedBlock],
    aligned: &[(NodeId, NodeId)],
    provenance: &HashMap<NodeId, BlockProvenance>,
) -> Result<(), String> {
    let base_controls = membership(base);
    let target_controls = membership(target);
    if base_controls.controls.is_empty() && target_controls.controls.is_empty() {
        return Ok(());
    }
    // Instances: source controls first, then target controls.
    let offset = base_controls.controls.len();
    let wrappers: Vec<_> = base_controls
        .controls
        .iter()
        .chain(&target_controls.controls)
        .cloned()
        .collect();
    let keys: Vec<ControlKey> = wrappers.iter().map(control_key).collect();
    let mut parent: Vec<usize> = (0..wrappers.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (a, b) = (find(parent, a), find(parent, b));
        if a != b {
            parent[b] = a;
        }
    }

    let base_path = |id: &NodeId| -> Result<Vec<usize>, String> {
        base_controls
            .paths
            .get(id)
            .cloned()
            .ok_or_else(|| format!("merged block {id} names no source block"))
    };
    let target_path = |id: &NodeId| -> Result<Vec<usize>, String> {
        Ok(target_controls
            .paths
            .get(id)
            .ok_or_else(|| format!("merged block {id} names no target block"))?
            .iter()
            .map(|instance| instance + offset)
            .collect())
    };
    for (base_id, target_id) in aligned {
        let (from, to) = (base_path(base_id)?, target_path(target_id)?);
        assert_eq!(
            from.len(),
            to.len(),
            "aligned blocks {base_id}/{target_id} sit in control chains of one shape"
        );
        for (a, b) in from.into_iter().zip(to) {
            union(&mut parent, a, b);
        }
    }

    // A merged block from the source keeps its source id; an inserted one is
    // named by its provenance.
    let mut paths = merged
        .iter()
        .map(|tracked| {
            let id = tracked_block_id(tracked);
            match provenance.get(id) {
                Some(BlockProvenance {
                    base_block_id: Some(base_id),
                    ..
                }) => base_path(base_id),
                Some(BlockProvenance {
                    base_block_id: None,
                    target_block_id: Some(target_id),
                }) => target_path(target_id),
                Some(BlockProvenance {
                    base_block_id: None,
                    target_block_id: None,
                }) => Err(format!("merged block {id} has an empty provenance")),
                None => base_path(id),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Join a source-only control and a target-only control with the same
    // properties where they meet under the same parents.
    let sided = |parent: &mut Vec<usize>| {
        let mut source = vec![false; wrappers.len()];
        let mut target = vec![false; wrappers.len()];
        for instance in 0..wrappers.len() {
            let root = find(parent, instance);
            if instance < offset {
                source[root] = true;
            } else {
                target[root] = true;
            }
        }
        (source, target)
    };
    loop {
        let (source, target) = sided(&mut parent);
        let mut joined = false;
        for index in 1..paths.len() {
            let (before, after) = (&paths[index - 1], &paths[index]);
            for depth in 0..before.len().min(after.len()) {
                let (x, y) = (
                    find(&mut parent, before[depth]),
                    find(&mut parent, after[depth]),
                );
                if x == y {
                    continue;
                }
                let one_sided_pair = (source[x] && !target[x] && target[y] && !source[y])
                    || (target[x] && !source[x] && source[y] && !target[y]);
                if one_sided_pair && keys[x] == keys[y] {
                    union(&mut parent, x, y);
                    joined = true;
                }
                break;
            }
        }
        if !joined {
            break;
        }
    }
    for path in &mut paths {
        for instance in path.iter_mut() {
            *instance = find(&mut parent, *instance);
        }
    }
    // Accept All reads the target, so a merged control takes the target's
    // properties when it has them.
    let mut merged_wrappers = wrappers.clone();
    for (instance, wrapper) in wrappers.iter().enumerate().skip(offset) {
        let root = find(&mut parent, instance);
        merged_wrappers[root] = wrapper.clone();
    }
    rebuild(merged, &paths, &merged_wrappers)
}
