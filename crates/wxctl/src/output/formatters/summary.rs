/// Summary statistics for an operation
#[derive(Debug, Default)]
pub struct OperationSummary {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub retained: usize,
    pub failed: usize,
    pub skipped_absent: usize,
    pub skipped_deferred: usize,
    pub undetermined: usize,
    pub total_duration_ms: u64,
    pub resource_urls: Vec<(String, String)>,
}

impl OperationSummary {
    /// Add decision to summary
    pub fn add_decision(&mut self, decision: &str) {
        match decision {
            "Create" | "CreateUnchecked" => self.created += 1,
            "Update" => self.updated += 1,
            "Delete" => self.deleted += 1,
            "Retain" => self.retained += 1,
            "SkipAbsent" => self.skipped_absent += 1,
            "SkipDeferred" => self.skipped_deferred += 1,
            "Undetermined" => self.undetermined += 1,
            _ => {}
        }
    }

    /// Add resource URL
    pub fn add_resource_url(&mut self, name: String, url: String) {
        self.resource_urls.push((name, url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `add_decision` routes each decision string to the right counter: the two
    /// skip labels land in distinct buckets, `Undetermined` counts separately,
    /// and `CreateUnchecked` folds into `created` alongside `Create`.
    #[test]
    fn add_decision_routes_labels_to_correct_counters() {
        let mut s = OperationSummary::default();
        for d in ["SkipAbsent", "SkipAbsent", "SkipDeferred", "Undetermined", "Create", "CreateUnchecked"] {
            s.add_decision(d);
        }
        assert_eq!(s.skipped_absent, 2, "two SkipAbsent");
        assert_eq!(s.skipped_deferred, 1, "one SkipDeferred");
        assert_eq!(s.undetermined, 1, "Undetermined counted separately");
        assert_eq!(s.created, 2, "Create + CreateUnchecked both bucket into created");
    }
}
