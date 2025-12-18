use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt::{Debug, Write},
    ops::Range,
};


#[derive(PartialEq, Eq, Debug, Clone, Copy, PartialOrd, Ord, Hash)]
pub enum RangeComp {
    // This range is fully left to Other range.
    Left,
    // This range is equal to other range.
    Equal,
    // This range fully overlaps other range.
    Overlap,
    // This range is within other range.
    Within,
    // This range is only partially intersects other range.
    NonComparable,
    // This range is fully right to Other range.
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

#[allow(dead_code)]
pub trait RangeExt {
    fn shift_left(&self, offset: usize) -> Self;
    fn shift_right(&self, offset: usize) -> Self;
    fn cmp_range(&self, other: impl Borrow<Range<usize>>) -> RangeComp;
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
    /// 3. If self fully contains other (starts before or at other's start and ends after or at other's end), it's Overlap Or Equal.
    /// 4. Otherwise, ranges partially intersect (NonComparable).
    fn cmp_range(&self, other: impl Borrow<Range<usize>>) -> RangeComp {
        let other = other.borrow();
        {
            if self.start == other.start && self.end == other.end {
                RangeComp::Equal
            } else if self.start <= other.start && self.end >= other.end {
                RangeComp::Overlap
            } else if self.start >= other.start && self.end <= other.end {
                RangeComp::Within
            } else if self.start >= other.end {
                RangeComp::Right
            } else if self.end <= other.start {
                RangeComp::Left
            } else {
                RangeComp::NonComparable
            }
        }
    }
}

impl RangeExt for wasmparser::RelocationEntry {
    fn shift_left(&self, offset: usize) -> Self {
        Self {
            offset: self.offset.checked_sub(offset as u32).unwrap(),
            ..*self
        }
    }

    fn shift_right(&self, offset: usize) -> Self {
        Self {
            offset: self.offset + offset as u32,
            ..*self
        }
    }

    fn cmp_range(&self, other: impl Borrow<Range<usize>>) -> RangeComp {
        self.relocation_range().cmp_range(other)
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

#[cfg(test)]
mod tests {

    use super::RangeExt;
    #[test]
    fn test_shift_range() {
        let range = 10..20;
        assert_eq!(range.shift_left(5), 5..15);
        assert_eq!(range.shift_right(5), 15..25);
    }

    #[test]
    fn test_cmp_range() {
        let range1 = 10..20;

        assert_eq!(range1.cmp_range(0..5), super::RangeComp::Right);
        assert_eq!(range1.cmp_range(0..10), super::RangeComp::Right);
        assert_eq!(range1.cmp_range(0..11), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(5..15), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(10..11), super::RangeComp::Overlap);
        assert_eq!(range1.cmp_range(10..20), super::RangeComp::Equal);
        assert_eq!(range1.cmp_range(15..20), super::RangeComp::Overlap);
        assert_eq!(range1.cmp_range(19..20), super::RangeComp::Overlap);
        assert_eq!(range1.cmp_range(15..25), super::RangeComp::NonComparable);
        assert_eq!(range1.cmp_range(20..35), super::RangeComp::Left);
        assert_eq!(range1.cmp_range(25..35), super::RangeComp::Left);

        assert_eq!(range1.cmp_range(2..40), super::RangeComp::Within);
    }

    #[test]
    fn test_cmp_range_from_file() {
        let range = 8916..8917;
        let other = 8916..8916;

        assert_eq!(range.cmp_range(other), super::RangeComp::Overlap);
    }

    #[test]
    fn test_partial_cmp() {
        let range = 10..20;

        assert_eq!(
            range.cmp_range(0..5).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            range.cmp_range(0..10).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(range.cmp_range(0..11).as_partial_ordering(), None);
        assert_eq!(range.cmp_range(5..15).as_partial_ordering(), None);
        assert_eq!(
            range.cmp_range(10..22).as_partial_ordering(),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            range.cmp_range(10..11).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            range.cmp_range(15..17).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            range.cmp_range(15..20).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            range.cmp_range(10..20).as_partial_ordering(),
            Some(std::cmp::Ordering::Equal)
        );

        assert_eq!(
            range.cmp_range(20..21).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );

        assert_eq!(
            range.cmp_range(35..40).as_partial_ordering(),
            Some(std::cmp::Ordering::Less)
        );

        let ranges = vec![0..5, 5..15, 6..11, 15..20, 20..35, 35..45];
        let mut res = ranges.clone();
        res.sort_by(|a, b| {
            a.cmp_range(b)
                .as_partial_ordering()
                .expect("Failed to compare ranges")
        });
        assert_eq!(res, ranges);
    }
}
