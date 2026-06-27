//! Core types used by the graph module.

use std::sync::Arc;

/// Interned string type for zero-cost cloning.
/// Uses Arc<str> for shared ownership without allocation on clone.
pub type IStr = Arc<str>;

/// Create an interned string from any string-like type.
#[inline]
pub fn istr(s: impl AsRef<str>) -> IStr {
    Arc::from(s.as_ref())
}

/// Resource identifier consisting of kind and name.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ResourceKey {
    pub kind: IStr,
    pub name: IStr,
}

impl ResourceKey {
    /// Create a new ResourceKey from string-like types.
    ///
    /// Both `kind` and `name` are converted to `Arc<str>` for zero-cost cloning.
    #[inline]
    pub fn new(kind: impl AsRef<str>, name: impl AsRef<str>) -> Self {
        Self { kind: istr(kind), name: istr(name) }
    }

    /// Create a ResourceKey from existing `IStr` values.
    ///
    /// Use this when you already have interned strings to avoid re-interning.
    #[inline]
    pub fn from_istr(kind: IStr, name: IStr) -> Self {
        Self { kind, name }
    }
}

impl std::fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)
    }
}
