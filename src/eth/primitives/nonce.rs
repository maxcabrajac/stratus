use std::fmt::Display;

use ethereum_types::U64;

use crate::derive_newtype_from;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nonce(U64);

impl Display for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// -----------------------------------------------------------------------------
// Conversions: Other -> Self
// -----------------------------------------------------------------------------
derive_newtype_from!(self = Nonce, other = u8, u16, u32, u64, usize);

// -----------------------------------------------------------------------------
// Conversions: Self -> Other
// -----------------------------------------------------------------------------
impl From<Nonce> for usize {
    fn from(value: Nonce) -> Self {
        value.0.as_usize()
    }
}

impl From<Nonce> for u64 {
    fn from(value: Nonce) -> Self {
        value.0.as_u64()
    }
}
