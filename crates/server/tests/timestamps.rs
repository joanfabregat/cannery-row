#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::timestamps::Timestamp;
use cannery_server::timestamps::{optional_timestamp, public_timestamp};
use chrono::{DateTime, FixedOffset};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn reference() -> Result<Vec<Value>> {
    let document: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/timestamp_reference.json"
    ))?;
    Ok(document["cases"]
        .as_array()
        .ok_or("missing source cases")?
        .clone())
}
fn timestamp(case: &Value) -> Result<Timestamp> {
    let seconds = case["epoch_seconds"].as_i64().ok_or("missing epoch")?;
    let micros = u32::try_from(
        case["microseconds"]
            .as_u64()
            .ok_or("missing microseconds")?,
    )?;
    let offset = i32::try_from(case["offset_seconds"].as_i64().ok_or("missing offset")?)?;
    let utc = DateTime::from_timestamp(seconds, micros * 1000).ok_or("invalid fixture epoch")?;
    let offset = FixedOffset::east_opt(offset).ok_or("invalid fixture offset")?;
    Ok(Timestamp(utc.with_timezone(&offset)))
}
fn public_fields(case: &Value) -> Result<()> {
    let formatted = public_timestamp(timestamp(case)?);
    let actual = json!(formatted);
    assert_eq!(actual, case["adapter"], "scalar {}", case["name"]);
    assert_eq!(actual, case["project"], "project {}", case["name"]);
    assert_eq!(
        actual, case["encoder_model"],
        "encoded model {}",
        case["name"]
    );
    for field in ["created_at", "disabled_at"] {
        assert_eq!(
            actual, case["service"][field],
            "service {field} {}",
            case["name"]
        );
    }
    for field in ["created_at", "expires_at", "last_used_at", "revoked_at"] {
        assert_eq!(
            actual, case["token"][field],
            "token {field} {}",
            case["name"]
        );
    }
    Ok(())
}
fn group(predicate: impl Fn(&str) -> bool, count: usize) -> Result<()> {
    let mut tested = 0;
    for case in reference()? {
        let name = case["name"].as_str().ok_or("missing name")?;
        if predicate(name) {
            public_fields(&case)?;
            tested += 1;
        }
    }
    assert_eq!(tested, count, "all intended source cases must run");
    Ok(())
}

#[test]
fn utc_calendar_boundaries_and_signed_zero_offsets() -> Result<()> {
    group(
        |name| !name.starts_with("offset-") && !name.starts_with("fraction-"),
        8,
    )
}
#[test]
fn nonzero_fractions_keep_six_digits_and_zero_fraction_is_absent() -> Result<()> {
    group(|name| name.starts_with("fraction-"), 8)
}
#[test]
fn positive_offset_seconds_preserve_local_clock_and_truncate_suffix() -> Result<()> {
    group(
        |name| name.starts_with("offset-") && !name.starts_with("offset--"),
        15,
    )
}
#[test]
fn negative_offset_seconds_keep_sign_even_below_one_minute() -> Result<()> {
    group(|name| name.starts_with("offset--"), 15)
}
#[test]
fn optional_fields_and_plain_json_dates_keep_their_source_context() -> Result<()> {
    let cases = reference()?;
    assert_eq!(cases.len(), 46);
    for case in cases {
        let value = timestamp(&case)?;
        assert_eq!(json!(optional_timestamp(Some(value))), case["adapter"]);
        for optional in case["optional_nulls"]
            .as_array()
            .ok_or("missing nullable observations")?
        {
            assert_eq!(json!(optional_timestamp(None)), *optional);
        }
        // Plain dictionaries/dataclasses and core audit strings use isoformat,
        // while typed Pydantic model projection uses the public formatter.
        let isoformat = json!(value.isoformat());
        assert_eq!(isoformat, case["isoformat"], "core {}", case["name"]);
        assert_eq!(isoformat, case["encoder_dict"], "dict {}", case["name"]);
        assert_eq!(
            isoformat, case["encoder_dataclass"],
            "dataclass {}",
            case["name"]
        );
        assert_eq!(serde_json::to_value(value)?, isoformat);
    }
    Ok(())
}
