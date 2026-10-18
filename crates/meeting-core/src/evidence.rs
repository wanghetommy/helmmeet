use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvidenceRef {
    pub segment_id: String,
    pub revision: u32,
}

/// `snapshot` must contain only the authorized meeting's exact submitted revisions.
pub fn validate_evidence(references: &[EvidenceRef], snapshot: &HashSet<EvidenceRef>) -> bool {
    !references.is_empty()
        && references.len() <= 32
        && references
            .iter()
            .all(|item| item.revision > 0 && snapshot.contains(item))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: &str, revision: u32) -> EvidenceRef {
        EvidenceRef {
            segment_id: id.into(),
            revision,
        }
    }
    #[test]
    fn rejects_cross_meeting_stale_and_fabricated_references() {
        let snapshot = HashSet::from([item("seg-1", 2)]);
        assert!(validate_evidence(&[item("seg-1", 2)], &snapshot));
        for invalid in [
            vec![],
            vec![item("seg-1", 1)],
            vec![item("other-meeting", 2)],
            vec![item("seg-1", 0)],
        ] {
            assert!(!validate_evidence(&invalid, &snapshot));
        }
    }
}
