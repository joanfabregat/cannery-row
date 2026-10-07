//! Upload bodies retain source declaration order and lossless integer coercion.
use crate::validation::{BodyInput, ValidationErrors};
use num_bigint::BigInt;

pub(crate) struct UploadRequest {
    pub role: String,
    pub name: String,
    pub size_bytes: BigInt,
    pub sha256: String,
    pub media_type: String,
}
pub(crate) fn upload(input: BodyInput<'_>) -> Result<UploadRequest, ValidationErrors> {
    crate::validation::validate_upload(input)
}
pub(crate) fn presign(input: BodyInput<'_>) -> Result<Option<Vec<BigInt>>, ValidationErrors> {
    crate::validation::validate_upload_presign(input)
}
