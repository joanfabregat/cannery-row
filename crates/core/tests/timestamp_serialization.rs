#![forbid(unsafe_code)]

use cannery_core::timestamps::Timestamp;

#[test]
fn postgres_text_preserves_python_fractional_precision_and_offsets()
-> Result<(), Box<dyn std::error::Error>> {
    for (input, expected) in [
        ("2025-01-09 14:00:00+00", "2025-01-09T14:00:00+00:00"),
        (
            "2025-01-09 14:00:00.123+00",
            "2025-01-09T14:00:00.123000+00:00",
        ),
        (
            "2025-01-09 14:00:00.000001+00",
            "2025-01-09T14:00:00.000001+00:00",
        ),
        (
            "2025-01-09 14:00:00.123456-07",
            "2025-01-09T14:00:00.123456-07:00",
        ),
        (
            "2025-01-09T14:00:00.000000+05:30",
            "2025-01-09T14:00:00+05:30",
        ),
        ("2025-01-09T14:00:00Z", "2025-01-09T14:00:00+00:00"),
    ] {
        let timestamp = input.parse::<Timestamp>()?;
        assert_eq!(timestamp.isoformat(), expected);
        assert_eq!(serde_json::to_value(timestamp)?, expected);
    }
    Ok(())
}
