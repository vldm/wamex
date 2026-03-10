use std::ops::{Range, RangeFrom, RangeTo};

use anyhow::Result;

///
/// Read-only cursor over a byte slice that focus on specific region.
/// It splits memory into three zones, green, red and grey.
///
/// Green zone is the area for which this cursor was created.
/// Red zone is the area that used by other cursors and therefore "untouchable".
/// grey zone is the area that is outside of both green and red zones,
/// and current that current cursor can extend into if needed.
///
///
/// The aim of this object is to handle overlapping of modification entries.
/// (mostly for debug and strict assertions)
pub struct Cursor<'any> {
    buffer: &'any [u8],
    green: Range<usize>,
    red_before: RangeTo<usize>,
    red_after: RangeFrom<usize>,
}

impl<'any> Cursor<'any> {
    // Create a new cursor with an empty buffer
    pub fn new(
        buffer: &'any [u8],
        green: Range<usize>,
        red_before: RangeTo<usize>,
        red_after: RangeFrom<usize>,
    ) -> Self {
        Self {
            buffer,
            green,
            red_before,
            red_after,
        }
    }

    // Get the current green zone
    pub fn green_buf(&self) -> &'any [u8] {
        &self.buffer[self.green.clone()]
    }
    // Try to extend the green zone before its current start.
    // By moving the start backwards into grey zone.
    //
    // Returns true if successful, false if blocked by red zone.
    pub fn try_extend_before(&mut self, shift_left: usize) -> Result<()> {
        let new_start = self.green.start.saturating_sub(shift_left);
        if new_start < self.red_before.end {
            return Err(anyhow::anyhow!("Already used by other modification"));
        }
        self.green.start = new_start;
        Ok(())
    }

    // Try to extend the green zone after its current end.
    // By moving the end forward into grey zone.
    // Returns true if successful, false if blocked by red zone.
    pub fn try_extend_after(&mut self, shift_right: usize) -> Result<()> {
        let new_end = self.green.end.saturating_add(shift_right);
        if new_end > self.red_after.start {
            return Err(anyhow::anyhow!("Already used by other modification"));
        }

        self.green.end = new_end;
        Ok(())
    }
}
