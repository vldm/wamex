//!
//! Neither wasm spec, nor llvm tooling describe a conception of virtual spaces.
//! Instead LLVM work with "main" memory and "`indirect_function_table`".
//!
//! So, during work with real files, we need to recover information about their virtual spaces.
//! This is important since we need to perform valid merging.
//!
//! We implement the following logic:
//! - For each segments that have continuos range - we place them in one virtual space.
//! - Segments from different files with same base location and flags are treated as same virtual space.
//!

use std::{collections::BTreeMap, ops::Range};

use cranelift_entity::EntityRef;

use crate::{
    helpers::cmp_range,
    layouts::{DataKind, SealedDataSegment, VirtualSpaceId},
    raw::SegmentId,
    typed::MemoryRef,
};

//
// Virtual space recover helpers
//
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
pub(super) struct VsKey {
    vs_location: DataKind<MemoryRef>,
    // Bump number to distinguish different virtual spaces with same base and got (e.g. multiple passive segments)
    // If segments cannot be merged (intersects) - try to keep their original offsets.
    bump_num: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VsState {
    range: Range<usize>,
    segments: Vec<SegmentId>,
    vs_id: VirtualSpaceId,
}

impl VsState {
    fn try_merge(&self, other: &Range<usize>, segment: SegmentId) -> Option<VsState> {
        let range = if self.range.end == other.start {
            self.range.start..other.end
        } else if self.range.start == other.end {
            other.start..self.range.end
        }
        // if intersects - panic
        else if cmp_range(&self.range, other).is_intersecting() {
            panic!(
                "Data segments with same base {:?} have intersecting ranges: {:?} and {:?}",
                self.vs_id, self.range, other
            );
        } else {
            return None;
        };
        let mut segments = self.segments.clone();
        segments.push(segment);
        Some(VsState {
            segments,
            range,
            vs_id: self.vs_id,
        })
    }
}

pub(super) struct VsRecover {
    // Map from virtual space key to its state.
    virtual_spaces: BTreeMap<VsKey, VsState>,
    next_vs_id: VirtualSpaceId,
}
impl VsRecover {
    pub fn new() -> Self {
        Self {
            virtual_spaces: BTreeMap::new(),
            next_vs_id: VirtualSpaceId::new(0),
        }
    }

    pub fn iter_vs(
        &self,
    ) -> impl Iterator<Item = (VirtualSpaceId, Vec<SegmentId>, DataKind<MemoryRef>)> + '_ {
        self.virtual_spaces
            .iter()
            .map(|(key, state)| (state.vs_id, state.segments.clone(), key.vs_location))
    }

    pub fn add_segment(&mut self, segment_id: SegmentId, segment: &SealedDataSegment<'_>) {
        let (mut key, offset) = Self::get_vs_key_base(segment);
        let range = offset..offset + segment.data_stream().bytes_len();
        let (key, state) = 'push: {
            for (existing, state) in self.iter_range(key.clone()) {
                if !existing.vs_location.is_active() {
                    break 'push (existing.clone(), state.clone());
                }
                // bump tmp key
                key.bump_num += 1;
                if let Some(merged) = state.try_merge(&range, segment_id) {
                    let existing = existing.clone();
                    break 'push (existing, merged);
                }
            }

            // not found - bump next_id and insert
            let temp_state = VsState {
                range,
                segments: vec![segment_id],
                vs_id: self.next_vs_id,
            };
            self.next_vs_id = self.next_vs_id.next();
            (key, temp_state)
        };

        self.virtual_spaces.insert(key, state);
    }

    fn iter_range(&self, base: VsKey) -> impl Iterator<Item = (&VsKey, &VsState)> + '_ {
        self.virtual_spaces
            .range(base.clone()..)
            .take_while(move |(k, _)| k.vs_location == base.vs_location)
    }

    // Get VsKey with bump = 0.
    fn get_vs_key_base(segment: &SealedDataSegment<'_>) -> (VsKey, usize) {
        match segment.va_address {
            DataKind::Active {
                memory_ref,
                location,
            } => {
                let offset = location.offset() as usize;
                let location = location.with_zero_offset();
                (
                    VsKey {
                        vs_location: DataKind::Active {
                            memory_ref,
                            location,
                        },
                        bump_num: 0,
                    },
                    offset,
                )
            }
            other => (
                VsKey {
                    vs_location: other,
                    bump_num: 0,
                },
                0,
            ),
        }
    }
}
