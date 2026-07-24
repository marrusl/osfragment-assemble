use std::collections::HashMap;

/// Result of repo deduplication analysis.
///
/// Maps repo IDs to the name of the canonical provider fragment.
/// Non-canonical providers should skip their repo COPY in the generated
/// Containerfile. Full implementation in Task 8.
#[derive(Debug, Clone)]
pub struct DeduplicationResult {
    /// repo_id -> canonical provider fragment name
    pub deduplicated_repos: HashMap<String, String>,
}
