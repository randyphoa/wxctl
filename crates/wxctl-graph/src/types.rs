//! Core types used by the graph module.

use std::sync::Arc;

/// Cheap-to-clone shared string (`Arc<str>`): cloning bumps a refcount instead
/// of copying the bytes. Not interned — there is no interning table, so equal
/// strings created separately do not share an allocation.
pub type IStr = Arc<str>;

/// Create an [`IStr`] from any string-like type (allocates once).
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
}

impl std::fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)
    }
}
