#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    start_ms: u64,
    end_ms: u64,
}

impl Interval {
    /// All recording intervals are half-open: [start_ms, end_ms).
    pub fn new(start_ms: u64, end_ms: u64) -> Option<Self> {
        (start_ms < end_ms).then_some(Self { start_ms, end_ms })
    }
    pub fn start_ms(self) -> u64 {
        self.start_ms
    }
    pub fn end_ms(self) -> u64 {
        self.end_ms
    }
}

/// Finds internal holes too; seeing the last timestamp does not imply full coverage.
pub fn missing_intervals(expected: Interval, processed: &[Interval]) -> Vec<Interval> {
    let mut sorted = processed.to_vec();
    sorted.sort_by_key(|item| item.start_ms);
    let mut cursor = expected.start_ms;
    let mut gaps = Vec::new();
    for item in sorted {
        let start = item.start_ms.max(expected.start_ms).min(expected.end_ms);
        let end = item.end_ms.min(expected.end_ms);
        if end <= cursor || start >= expected.end_ms {
            continue;
        }
        if start > cursor {
            gaps.push(Interval::new(cursor, start).unwrap());
        }
        cursor = cursor.max(end);
    }
    if cursor < expected.end_ms {
        gaps.push(Interval::new(cursor, expected.end_ms).unwrap());
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    fn range(a: u64, b: u64) -> Interval {
        Interval::new(a, b).unwrap()
    }
    #[test]
    fn reports_internal_holes_despite_final_timestamp() {
        assert_eq!(
            missing_intervals(range(0, 100), &[range(70, 100), range(0, 30)]),
            vec![range(30, 70)]
        );
    }
    #[test]
    fn merges_overlap_adjacency_and_clips_to_request() {
        assert!(missing_intervals(
            range(10, 90),
            &[range(0, 40), range(30, 70), range(70, 120)]
        )
        .is_empty());
    }
    #[test]
    fn no_data_and_outside_data_do_not_hide_missing_audio() {
        assert_eq!(
            missing_intervals(range(10, 20), &[range(0, 5), range(30, 40)]),
            vec![range(10, 20)]
        );
        assert_eq!(missing_intervals(range(0, 10), &[]), vec![range(0, 10)]);
        assert!(Interval::new(10, 10).is_none());
        assert!(Interval::new(20, 10).is_none());
    }
}
