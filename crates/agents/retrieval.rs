//! Agent-facing knowledge retrieval contract.
//!
//! Agents consume typed retrieval results and do not know whether candidates
//! came from SQLite FTS, a deterministic fallback, or a future implementation.

use models::{KnowledgeRetrievalQuery, KnowledgeRetrievalResult};

/// Retrieves tactical knowledge for planning.
pub trait KnowledgeRetriever {
    /// Backend-specific failure type.
    type Error;

    /// Retrieve ranked knowledge for a normalized query contract.
    ///
    /// # Errors
    /// Returns the backend error without fabricating results.
    fn retrieve(
        &self,
        query: &KnowledgeRetrievalQuery,
    ) -> Result<Vec<KnowledgeRetrievalResult>, Self::Error>;
}
