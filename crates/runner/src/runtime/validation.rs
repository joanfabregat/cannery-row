//! Native pre-upload validation shared with the receiving server.
use super::{FutureResult, RuntimeError, worker::OutputValidator};

use cannery_research::{
    interfaces::{ContentChecker, Interface, ValidationContext},
    science,
};
use serde_json::Value;
use std::path::Path;
use tokio::io::AsyncReadExt;

/// Explicit native-runtime resource policy. This is not a universal Python
/// call-stack calibration; it bounds schema execution and retained JSON bytes.
pub struct NativeOutputValidator {
    pub context: ValidationContext,
    pub json_max_bytes: usize,
}
impl NativeOutputValidator {
    #[must_use]
    pub fn runtime_policy() -> Self {
        Self {
            context: ValidationContext {
                json_decode_budget: 128,
            },
            json_max_bytes: 64 * 1024 * 1024,
        }
    }
}
impl OutputValidator for NativeOutputValidator {
    fn check<'a>(
        &'a self,
        reference: Option<&'a str>,
        content: &'a Value,
        file: &'a Path,
    ) -> FutureResult<'a, ()> {
        Box::pin(async move {
            let Some(reference) = reference else {
                return Ok(());
            };
            if reference == "cr-evidence/v0.2" || reference == super::worker::RUN_INTERFACE {
                return Ok(());
            }
            let bytes = serde_json::to_vec(content)?;
            let document = cannery_core::json::decode(&bytes, self.context.json_decode_budget)
                .map_err(|_| RuntimeError::Contract)?;
            let projection = science::Science::new(
                0.into(),
                &document,
                science::RenderingContext {
                    nesting_budget: self.context.json_decode_budget,
                },
            )
            .map_err(|_| RuntimeError::Contract)?;
            let spec = projection
                .interface_specs
                .iter()
                .find(|(key, _)| key.equals_utf8(reference))
                .map(|(_, spec)| spec)
                .ok_or(RuntimeError::Contract)?;
            let interface =
                Interface::from_spec(&document, spec).map_err(|_| RuntimeError::Contract)?;
            let mut checker = ContentChecker::new(interface, self.json_max_bytes, self.context)
                .map_err(|_| RuntimeError::Contract)?;
            let mut input = tokio::fs::File::open(file).await?;
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let size = input.read(&mut buffer).await?;
                if size == 0 {
                    break;
                }
                checker
                    .feed(&buffer[..size])
                    .map_err(|_| RuntimeError::InvalidStepOutput)?;
            }
            if checker
                .finish()
                .map_err(|_| RuntimeError::InvalidStepOutput)?
                .is_empty()
            {
                Ok(())
            } else {
                Err(RuntimeError::InvalidStepOutput)
            }
        })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
