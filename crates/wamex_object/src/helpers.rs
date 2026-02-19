use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt::{Debug, Write},
    ops::{Add, Range, Sub},
};

#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord, Hash)]
pub enum RangeComp {
    /// This range is fully left to Other range.
    Left,
    /// This range is equal to other range.
    Equal,
    /// This range fully overlaps other range.
    Overlap,
    /// This range is within other range.
    Within,
    /// This range is only partially intersects other range.
    NonComparable,
    /// This range is fully right to Other range.
    Right,
}

#[allow(dead_code)]
impl RangeComp {
    /// Converts the RangeComp to a PartialOrd, usefull for sorting ranges.
    /// Returns None if the RangeComp is NonComparable.
    pub fn as_partial_ordering(&self) -> Option<Ordering> {
        match self {
            RangeComp::Left | RangeComp::Overlap => Some(Ordering::Less),
            RangeComp::Equal => Some(Ordering::Equal),
            RangeComp::Right | RangeComp::Within => Some(Ordering::Greater),
            _ => None,
        }
    }
}

/// Compares two ranges and returns their relationship.
///
/// Precedence/order of checks:
/// 1. If self starts at or after other's end, self is fully to the right.
/// 2. If self ends at or before other's start, self is fully to the left.
/// 3. If self fully contains other (starts before or at other's start and ends after or at other's end), it's Overlap Or Equal.
/// 4. Otherwise, ranges partially intersect (NonComparable).
pub fn cmp_range<V>(some: &Range<V>, other: impl Borrow<Range<V>>) -> RangeComp
where
    V: Ord,
{
    let other = other.borrow();
    {
        if some.start == other.start && some.end == other.end {
            RangeComp::Equal
        } else if some.start <= other.start && some.end >= other.end {
            RangeComp::Overlap
        } else if some.start >= other.start && some.end <= other.end {
            RangeComp::Within
        } else if some.start >= other.end {
            RangeComp::Right
        } else if some.end <= other.start {
            RangeComp::Left
        } else {
            RangeComp::NonComparable
        }
    }
}

#[allow(dead_code)]
pub trait RangeExt {
    fn shift_left(&self, offset: usize) -> Self;
    fn shift_right(&self, offset: usize) -> Self;
    fn shift(&self, offset: isize) -> Self
    where
        Self: Sized,
    {
        if offset >= 0 {
            self.shift_right(offset as usize)
        } else {
            self.shift_left((-offset) as usize)
        }
    }
}

impl RangeExt for Range<usize> {
    fn shift_left(&self, offset: usize) -> Self {
        Range {
            start: self.start.checked_sub(offset).unwrap(),
            end: self.end.checked_sub(offset).unwrap(),
        }
    }

    fn shift_right(&self, offset: usize) -> Self {
        Range {
            start: self.start + offset,
            end: self.end + offset,
        }
    }
}

/// Debug formating for mostly filled slices.
/// Input slice should be ordered, and will be printed in lines of `max_elements` elements.
/// If some elements are skipped, it will print `placeholder` in place of skipped element.
/// Returns dyn Debug formatter.
pub fn debug_fmt_mostly_filled<T: Debug>(
    slice: &[T],
    width: usize,
    max_elements: usize,
    placeholder: &str,
    skipped: impl Fn(&T, &T) -> bool,
) -> impl Debug {
    struct DebugFmt {
        result: String,
    }
    impl Debug for DebugFmt {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.result)?;
            Ok(())
        }
    }
    let mut writer = DebugFmt {
        result: String::new(),
    };
    let placeholder = format!("{placeholder:>width$} ");

    let mut last = None;
    let mut shift = 0;
    for (i, item) in slice.iter().enumerate() {
        if let Some(last_item) = last {
            if skipped(last_item, item) {
                writer.result.write_str(&placeholder).unwrap();
                shift += 1;
            }
        }
        if (i + shift) % max_elements == 0 {
            writer.result.write_char('\n').unwrap();
        }
        writer
            .result
            .write_str(&format!("{item:>width$?} "))
            .unwrap();

        last = Some(item);
    }
    writer.result.pop(); // Remove last space
    writer
}

pub fn encoding_size(n: u32) -> usize {
    let (_value, pos) = leb128fmt::encode_u32(n).unwrap();
    pos
}

pub fn demangle_full(name: &str) -> String {
    rustc_demangle::demangle(name).to_string()
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, PartialOrd, Ord)]
pub struct ShiftPoint<Offset> {
    pub at: Offset,
    pub shift: i32,
}

impl<Offset> ShiftPoint<Offset> {
    pub fn remove(at: Offset, shift: u32) -> Self {
        let remove: i32 = shift.try_into().unwrap();
        Self { at, shift: -remove }
    }
    pub fn insert(at: Offset, shift: u32) -> Self {
        Self {
            at,
            shift: shift.try_into().unwrap(),
        }
    }
}

#[derive(Default, Debug, Eq, PartialEq, Clone, Copy, PartialOrd, Ord)]
struct Point<Offset> {
    at: Offset,
    shift: i32,
    removed_size: Option<u32>,
}

#[derive(Debug, Eq, PartialEq, Clone, PartialOrd, Ord)]
pub struct ShiftMap<Offset> {
    points: Vec<Point<Offset>>,
}

impl<Offset> ShiftMap<Offset>
where
    Offset: Ord,
{
    pub fn new() -> Self {
        Self { points: vec![] }
    }
}

// TODO: add support usize offset.
impl<Offset> ShiftMap<Offset>
where
    Offset: Ord + Copy + Add<u32, Output = Offset> + Debug,
{
    /// Build ShiftMap from points, accumulating shifts.
    /// Expects no duplicate offsets in points.
    pub fn build(points: Vec<ShiftPoint<Offset>>) -> Self
    where
        Offset: Default,
    {
        // ord points by offset
        let mut sorted_points = points
            .into_iter()
            .map(|p| {
                let removed_size = (p.shift < 0).then(|| (-p.shift) as u32);
                Point {
                    at: p.at,
                    shift: p.shift,
                    removed_size,
                }
            })
            .collect::<Vec<_>>();

        sorted_points.sort_by(|a, b| a.at.cmp(&b.at));

        // update shifts with accumulator
        let mut last_offset = None;
        let mut accumulated_shift = 0;
        for point in &mut sorted_points {
            point.shift += accumulated_shift;
            accumulated_shift = point.shift;

            if let Some((last_offset, last_removed)) = last_offset {
                assert!(point.at > last_offset, "Duplicate shift points");
                if let Some(removed_size) = last_removed {
                    let removed_range = last_offset..last_offset + removed_size;
                    if removed_range.contains(&point.at) {
                        panic!(
                            "Shift point at {point:?} is in range of removed data {removed_range:?}"
                        );
                    }
                }
            }
            last_offset = Some((point.at, point.removed_size));
        }

        ShiftMap {
            points: sorted_points,
        }
    }
    /// Add a shift point, updating accumulated shifts.
    /// Panics if a duplicate point is added.
    fn add_point_inner(&mut self, point: Point<Offset>) {
        assert!(
            point.shift != 0,
            "Cannot add shift point with zero shift: {point:?}"
        );
        // Make sure no duplicate points exist
        let partition = self
            .points
            .binary_search_by(|v| v.at.cmp(&point.at))
            .err()
            .expect("Duplicate shift points");

        // Accumulate shifts before insertion point
        let last_shift = match partition
            .checked_sub(1)
            .and_then(|idx| self.points.get(idx))
        {
            None => 0,
            Some(p) => {
                // If we have removed size, ensure that current point is not in that range.
                if let Some(removed_size) = p.removed_size {
                    let removed_range = p.at..p.at + removed_size;
                    if removed_range.contains(&point.at) {
                        panic!(
                            "Cannot add shift point at {point:?}, it is in range of removed data {removed_range:?}"
                        );
                    }
                }
                p.shift
            }
        };

        // And if we have removed size, ensure that next point is not in that range.
        if let Some(removed_size) = point.removed_size {
            let removed_range = point.at..point.at + removed_size;
            if let Some(next_point) = self.points.get(partition) {
                if removed_range.contains(&next_point.at) {
                    panic!(
                        "Cannot add shift point at {point:?}, next point {next_point:?} is in range of removed data {removed_range:?}"
                    );
                }
            }
        }

        // And after all checks - we can insert new point
        self.points.insert(
            partition,
            Point {
                at: point.at,
                shift: last_shift,
                removed_size: point.removed_size,
            },
        );

        // And update all shifts including newly added point
        for p in &mut self.points[partition..] {
            p.shift += point.shift;
        }
    }

    pub fn add_shift_point(&mut self, point: ShiftPoint<Offset>) {
        let removed_size = (point.shift < 0).then(|| (-point.shift) as u32);
        self.add_point_inner(Point {
            at: point.at,
            shift: point.shift,
            removed_size,
        });
    }
    pub fn insert(&mut self, at: Offset, shift: u32) {
        self.add_shift_point(ShiftPoint {
            at,
            shift: shift.try_into().unwrap(),
        });
    }
    pub fn remove(&mut self, at: Offset, removed_size: u32) {
        let shift: i32 = removed_size.try_into().unwrap();
        self.add_shift_point(ShiftPoint { at, shift: -shift });
    }

    /// Get the accumulated shift at the given offset.
    ///
    ///
    /// Return None if offset was removed.
    pub fn get_shift_raw(&self, offset: Offset) -> Option<i32> {
        let point = match self.points.binary_search_by(|v| v.at.cmp(&offset)) {
            Ok(index) => &self.points[index],
            Err(0) => return Some(0),
            Err(index) => &self.points[index - 1],
        };
        if let Some(removed_size) = point.removed_size {
            let removed_range = point.at..point.at + removed_size;
            if removed_range.contains(&offset) {
                return None;
            }
        }
        Some(point.shift)
    }

    pub fn get_shifted_offset(&self, offset: Offset) -> Option<Offset>
    where
        Offset: Sub<u32, Output = Offset>,
    {
        self.get_shift_raw(offset).map(|shift| {
            if shift < 0 {
                offset - (-shift) as u32
            } else {
                offset + shift as u32
            }
        })
    }
    pub fn cursor_at(&self, offset: Offset) -> ShiftCursor<'_, Offset>
    where
        Offset: Default,
    {
        let current_index = self
            .points
            .binary_search_by(|v| v.at.cmp(&offset))
            .unwrap_or_else(|e| e);

        ShiftCursor {
            shift_map: self,
            current_index,
        }
    }
}

/// For cases when we have a lot of sequential queries to `ShiftMap`, we can avoid binary search each time.
pub struct ShiftCursor<'a, Offset> {
    shift_map: &'a ShiftMap<Offset>,
    current_index: usize,
}

impl<'a, Offset> ShiftCursor<'a, Offset>
where
    Offset: Ord + Copy + Add<u32, Output = Offset> + Debug,
{
    /// Get the accumulated shift at the given offset.
    ///
    ///
    /// Return None if offset was removed.
    pub fn get_shift_raw(&mut self, offset: Offset) -> Option<i32> {
        // Update cursor if next element behind offset
        while let Some(next_point) = self.shift_map.points.get(self.current_index + 1) {
            // No need to update cursor if next point is still before offset
            if next_point.at > offset {
                break;
            }
            self.current_index += 1;
        }

        let Some(point) = &self.shift_map.points.get(self.current_index) else {
            return Some(0);
        };
        if point.at > offset {
            return Some(0);
        }

        // Check if offset is in removed range
        if let Some(removed_size) = point.removed_size {
            let removed_range = point.at..point.at + removed_size;
            if removed_range.contains(&offset) {
                return None;
            }
        }
        Some(point.shift)
    }

    /// Get new shifted offset at given offset.
    pub fn get_shifted_offset(&mut self, offset: Offset) -> Option<Offset>
    where
        Offset: Sub<u32, Output = Offset>,
    {
        self.get_shift_raw(offset).map(|shift| {
            if shift < 0 {
                offset - (-shift) as u32
            } else {
                offset + shift as u32
            }
        })
    }
}

#[cfg(test)]
mod tests {

    use super::{RangeExt, ShiftMap, ShiftPoint};
    use crate::helpers::cmp_range;
    #[test]
    fn test_shift_range() {
        let range = 10..20;
        assert_eq!(range.shift_left(5), 5..15);
        assert_eq!(range.shift_right(5), 15..25);
    }

    #[test]
    fn test_cmp_range() {
        let range1 = 10..20;

        assert_eq!(cmp_range(&range1, 0..5), super::RangeComp::Right);
        assert_eq!(cmp_range(&range1, 0..10), super::RangeComp::Right);
        assert_eq!(cmp_range(&range1, 0..11), super::RangeComp::NonComparable);
        assert_eq!(cmp_range(&range1, 5..15), super::RangeComp::NonComparable);
        assert_eq!(cmp_range(&range1, 10..11), super::RangeComp::Overlap);
        assert_eq!(cmp_range(&range1, 10..20), super::RangeComp::Equal);
        assert_eq!(cmp_range(&range1, 15..20), super::RangeComp::Overlap);
        assert_eq!(cmp_range(&range1, 19..20), super::RangeComp::Overlap);
        assert_eq!(cmp_range(&range1, 15..25), super::RangeComp::NonComparable);
        assert_eq!(cmp_range(&range1, 20..35), super::RangeComp::Left);
        assert_eq!(cmp_range(&range1, 25..35), super::RangeComp::Left);

        assert_eq!(cmp_range(&range1, 2..40), super::RangeComp::Within);
    }

    #[test]
    fn test_cmp_range_from_file() {
        let range = 8916..8917;
        let other = 8916..8916;

        assert_eq!(cmp_range(&range, other), super::RangeComp::Overlap);
    }

    #[test]
    fn test_partial_cmp() {
        let range = 10..20;

        assert_eq!(
            cmp_range(&range, 0..5).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            cmp_range(&range, 0..10).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(cmp_range(&range, 0..11).as_partial_ordering(), None);
        assert_eq!(cmp_range(&range, 5..15).as_partial_ordering(), None);
        assert_eq!(
            cmp_range(&range, 10..22).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            cmp_range(&range, 10..11).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            cmp_range(&range, 15..17).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            cmp_range(&range, 15..20).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            cmp_range(&range, 10..20).as_partial_ordering(),
            Some(std::cmp::Ordering::Equal)
        );

        assert_eq!(
            cmp_range(&range, 20..21).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );

        assert_eq!(
            cmp_range(&range, 35..40).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );

        let ranges = vec![0..5, 5..15, 6..11, 15..20, 20..35, 35..45];
        let mut res = ranges.clone();
        res.sort_by(|a, b| {
            cmp_range(a, b)
                .as_partial_ordering()
                .expect("Failed to compare ranges")
        });
        assert_eq!(res, ranges);
    }

    #[test]
    fn shift_map() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: 5,
            },
            ShiftPoint {
                at: 15u32,
                shift: 2,
            },
            ShiftPoint {
                at: 20u32,
                shift: -3,
            },
            ShiftPoint {
                at: 30u32,
                shift: 4,
            },
        ];

        let shift_map = ShiftMap::build(points);

        for any_offset in 0..10 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 0);
        }
        for any_offset in 10..15 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 5);
        }
        for any_offset in 15..20 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 7);
        }
        // removed range
        for any_offset in 20..23 {
            assert!(shift_map.get_shift_raw(any_offset).is_none());
        }
        for any_offset in 23..30 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 4);
        }
        for any_offset in 30..40 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 8);
        }

        let other_map = {
            let mut map = ShiftMap::new();
            map.insert(10u32, 5);
            map.insert(15u32, 2);
            map.remove(20u32, 3);
            map.insert(30u32, 4);
            map
        };

        assert_eq!(shift_map, other_map);
    }

    #[test]
    fn test_shift_map_cursor() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: 5,
            },
            ShiftPoint {
                at: 15u32,
                shift: 2,
            },
            ShiftPoint {
                at: 20u32,
                shift: -3,
            },
            ShiftPoint {
                at: 30u32,
                shift: 4,
            },
        ];

        let shift_map = ShiftMap::build(points);
        let mut cursor = shift_map.cursor_at(0u32);
        for any_offset in 0..10 {
            assert_eq!(cursor.get_shift_raw(any_offset).unwrap(), 0);
        }
        for any_offset in 10..15 {
            assert_eq!(cursor.get_shift_raw(any_offset).unwrap(), 5);
        }
        for any_offset in 15..20 {
            assert_eq!(cursor.get_shift_raw(any_offset).unwrap(), 7);
        }
        // removed range
        for any_offset in 20..23 {
            assert!(shift_map.get_shift_raw(any_offset).is_none());
        }
        for any_offset in 23..30 {
            assert_eq!(shift_map.get_shift_raw(any_offset).unwrap(), 4);
        }
        for any_offset in 30..40 {
            assert_eq!(cursor.get_shift_raw(any_offset).unwrap(), 8);
        }
    }

    #[test]
    #[should_panic(expected = "Duplicate shift points")]
    fn shift_map_duplicate_point_build() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: 5,
            },
            ShiftPoint {
                at: 10u32,
                shift: 2,
            },
        ];

        let _shift_map = ShiftMap::build(points);
    }
    #[test]
    #[should_panic(expected = "Duplicate shift points")]
    fn shift_map_duplicate_point() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: 5,
            },
            ShiftPoint {
                at: 10u32,
                shift: 2,
            },
        ];

        let mut shift_map = ShiftMap::new();
        for point in points {
            shift_map.add_shift_point(point);
        }
    }

    #[test]
    #[should_panic(expected = "is in range of removed data")]
    fn shift_map_point_in_removed_range() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: -5,
            },
            ShiftPoint {
                at: 12u32,
                shift: 2,
            },
        ];
        let mut shift_map = ShiftMap::new();
        for point in points {
            shift_map.add_shift_point(point);
        }
    }

    #[test]
    #[should_panic(expected = "is in range of removed data")]
    fn shift_map_point_in_removed_range_build() {
        let points = vec![
            ShiftPoint {
                at: 10u32,
                shift: -5,
            },
            ShiftPoint {
                at: 12u32,
                shift: 2,
            },
        ];
        let _shift_map = ShiftMap::build(points);
    }

    #[test]
    #[should_panic(expected = "is in range of removed data")]
    fn shift_map_next_point_in_removed_range() {
        let points = vec![
            ShiftPoint {
                at: 14u32,
                shift: 2,
            },
            ShiftPoint {
                at: 10u32,
                shift: -5,
            },
        ];
        let mut shift_map = ShiftMap::new();
        for point in points {
            shift_map.add_shift_point(point);
        }
    }

    #[test]
    #[should_panic(expected = "is in range of removed data")]
    fn shift_map_next_point_in_removed_range_build() {
        let points = vec![
            ShiftPoint {
                at: 14u32,
                shift: 2,
            },
            ShiftPoint {
                at: 10u32,
                shift: -5,
            },
        ];
        let _shift_map = ShiftMap::build(points);
    }

    #[test]
    fn shift_map_test_symbol_id_usage() {
        let symbols = (0u32..30).collect::<Vec<_>>();
        let removed = [10, 15, 21]; // arbitrary removed symbols

        let mut shift_map = ShiftMap::new();
        for &sym in &removed {
            shift_map.remove(sym, 1); // remove ony at once
        }

        let mut symbol_map = std::collections::BTreeMap::<u32, u32>::new();
        let mut rebuild_symbols = Vec::new();

        // usage of shiftmap allows remap symbol ids after removals
        for &sym in &symbols {
            let Some(shift) = shift_map.get_shift_raw(sym) else {
                continue; // symbol was removed
            };
            let new_sym = (sym as i32 + shift) as u32;
            rebuild_symbols.push(new_sym);
            symbol_map.insert(sym, new_sym);
        }

        let expected_results = (0u32..27).collect::<Vec<_>>();
        assert_eq!(rebuild_symbols, expected_results);

        let expected_map = {
            let expected_from = (0u32..10)
                .chain(11u32..15)
                .chain(16u32..21)
                .chain(22u32..30)
                .collect::<Vec<_>>();
            expected_from
                .into_iter()
                .zip(expected_results.into_iter())
                .collect()
        };
        assert_eq!(symbol_map, expected_map);
    }
}
