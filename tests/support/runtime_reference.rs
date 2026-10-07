//! Read a freshly exported Python reference when the test runs, never at build time.

#[allow(clippy::panic)]
pub fn read(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(reference) => reference,
        Err(error) => panic!(
            "reference {path} is unavailable: {error}; generate it with the frozen Python exporter before running this test"
        ),
    }
}

macro_rules! runtime_reference {
    ($path:literal) => {{
        static REFERENCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        REFERENCE
            .get_or_init(|| {
                $crate::runtime_reference::read(concat!(env!("CARGO_MANIFEST_DIR"), $path))
            })
            .as_str()
    }};
}
