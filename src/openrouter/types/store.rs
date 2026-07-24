use std::fmt;

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterStoreFailureCategory {
    Read,
    Write,
    Delete,
    Permissions,
    Corrupt,
    UnsupportedVersion,
    ResourceLimit,
    NotFound,
}

#[derive(Clone, Copy, Error, Eq, PartialEq)]
#[error("OpenRouter local storage failed ({category:?})")]
pub struct OpenRouterStoreError {
    category: OpenRouterStoreFailureCategory,
}

impl OpenRouterStoreError {
    pub fn new(category: OpenRouterStoreFailureCategory) -> Self {
        Self { category }
    }

    pub fn category(self) -> OpenRouterStoreFailureCategory {
        self.category
    }
}

impl fmt::Debug for OpenRouterStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterStoreError")
            .field("category", &self.category)
            .finish()
    }
}
