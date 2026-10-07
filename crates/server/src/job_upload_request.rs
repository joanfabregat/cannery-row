use num_bigint::BigInt;
pub(crate) struct Request {
    pub role: String,
    pub path: String,
    pub size_bytes: BigInt,
    pub sha256: String,
    pub media_type: String,
    pub interface: Option<String>,
}
