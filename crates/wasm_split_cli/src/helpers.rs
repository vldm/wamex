use std::{
    borrow::Borrow,
    fmt::{Debug, Write},
    ops::Range,
};

#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord, Hash)]
pub enum RangeComp {
    // This range is equal, or fully overlaps other range.
    OverlapOrEqual,
    // This range is only partially intersects other range.
    NonComparable,
    // This range is fully right to Other range.
    Right,
    // This range is fully left to Other range.
    Left,
}

pub trait RangeExt {
    fn shift_left(&self, offset: usize) -> Self;
    fn shift_right(&self, offset: usize) -> Self;
    fn cmp_range(&self, other: impl Borrow<Self>) -> RangeComp;
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

    /// Compares two ranges and returns their relationship.
    ///
    /// Precedence/order of checks:
    /// 1. If self starts at or after other's end, self is fully to the right.
    /// 2. If self ends at or before other's start, self is fully to the left.
    /// 3. If self fully contains other (starts before or at other's start and ends after or at other's end), it's OverlapOrEqual.
    /// 4. Otherwise, ranges partially intersect (NonComparable).
    fn cmp_range(&self, other: impl Borrow<Self>) -> RangeComp {
        let other = other.borrow();
        {
            if self.start >= other.end {
                RangeComp::Right
            } else if self.end <= other.start {
                RangeComp::Left
            } else if self.start <= other.start && self.end >= other.end {
                RangeComp::OverlapOrEqual
            } else {
                RangeComp::NonComparable
            }
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

/// Returns an iterator if the condition is true, otherwise returns an empty iterator.
pub fn iter_if<T>(condition: bool, iter: impl Iterator<Item = T>) -> impl Iterator<Item = T> {
    condition.then_some(iter).into_iter().flatten()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_shift_range() {
        use super::RangeExt;
        let range = 10..20;
        assert_eq!(range.shift_left(5), 5..15);
        assert_eq!(range.shift_right(5), 15..25);
    }

    #[test]
    fn test_cmp_range() {
        use super::RangeExt;
        let range1 = 10..20;

        assert_eq!(range1.cmp_range(0..5), super::RangeComp::Right);
        assert_eq!(range1.cmp_range(0..10), super::RangeComp::Right);
        assert_eq!(range1.cmp_range(0..11), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(5..15), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(10..11), super::RangeComp::OverlapOrEqual);
        assert_eq!(range1.cmp_range(10..20), super::RangeComp::OverlapOrEqual);
        assert_eq!(range1.cmp_range(15..20), super::RangeComp::OverlapOrEqual);
        assert_eq!(range1.cmp_range(19..20), super::RangeComp::OverlapOrEqual);
        assert_eq!(range1.cmp_range(15..25), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(20..35), super::RangeComp::Left);
        assert_eq!(range1.cmp_range(25..35), super::RangeComp::Left);
    }
}
