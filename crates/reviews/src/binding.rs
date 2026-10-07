//! Shared source-measured binary integer adaptation with domain error mapping.
pub use cannery_core::pg_integer::Integer;
impl From<cannery_core::pg_integer::IntegerError> for crate::Error {
    fn from(_: cannery_core::pg_integer::IntegerError) -> Self {
        Self::Driver
    }
}
