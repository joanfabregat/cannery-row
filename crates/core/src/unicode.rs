//! Unicode classification supplied by the Rust standard library.
#[must_use]
pub fn word(point: u32) -> bool {
    char::from_u32(point).is_some_and(|c| c == '_' || c.is_alphanumeric())
}
#[must_use]
pub fn nvidia_prefix(text: &str) -> bool {
    text.to_uppercase().starts_with("NVIDIA_")
}
