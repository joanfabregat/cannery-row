#![allow(clippy::unwrap_used)]
use cannery_runner::launcher::{ContractError, quantity};
use num_bigint::BigInt;
use num_rational::BigRational;

#[test]
fn resource_quantities_use_bounded_ascii_and_exact_arithmetic() {
    assert_eq!(
        quantity("0.001Gi").unwrap(),
        BigRational::new(BigInt::from(1u64 << 30), 1000.into())
    );
    assert_eq!(
        quantity("1001m").unwrap(),
        BigRational::new(1001.into(), 1000.into())
    );
    for invalid in [
        "1\n", "1\r\n", " 1", "1 ", "１", "١", "1e3", "1.", "+1", "-1",
    ] {
        assert_eq!(quantity(invalid), Err(ContractError::Quantity));
    }
    assert!(quantity(&"9".repeat(1024)).is_ok());
    assert_eq!(
        quantity(&"9".repeat(1025)),
        Err(ContractError::IntegerLimit)
    );
    assert!(quantity(&format!("{}Ki", "9".repeat(1022))).is_ok());
    assert_eq!(
        quantity(&format!("{}Ki", "9".repeat(1023))),
        Err(ContractError::IntegerLimit)
    );
}

#[test]
fn pinned_images_reject_line_endings_and_native_paths_preserve_utf8() {
    use cannery_runner::launcher::{PosixPath, host_path, pinned_image};
    use std::path::Path;
    let image = format!(
        "registry.example:5000/team/worker:tag@sha256:{}",
        "a".repeat(64)
    );
    assert_eq!(
        pinned_image(&image).unwrap().0,
        "registry.example:5000/team/worker"
    );
    assert_eq!(
        pinned_image(&format!("{image}\n")),
        Err(ContractError::Image)
    );
    assert_eq!(
        pinned_image(&format!("{image}\r\n")),
        Err(ContractError::Image)
    );
    assert_eq!(PosixPath::new("/cr/é/./𐀀").text(), "/cr/é/𐀀");
    assert_eq!(
        host_path(Path::new("/native"), "/cr/é/𐀀").unwrap(),
        Path::new("/native/é/𐀀")
    );
}
