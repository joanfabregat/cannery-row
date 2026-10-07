//! Frozen Pydantic datetime conversion for UTF-8 query parameter values.
use cannery_core::timestamps::Timestamp;
use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use speedate::{
    Date, DateConfig, DateTime, DateTimeConfig, MicrosecondsPrecisionOverflowBehavior, TimeConfig,
    TimestampUnit,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ParsedDateTime {
    Aware(Timestamp),
    Naive(NaiveDateTime),
}
impl std::fmt::Debug for ParsedDateTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ParsedDateTime(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateTimeQueryError {
    DateTimeParsing,
    DateTimeFromDateParsing,
}
impl DateTimeQueryError {
    #[must_use]
    pub const fn source_category(self) -> &'static str {
        match self {
            Self::DateTimeParsing => "datetime_parsing",
            Self::DateTimeFromDateParsing => "datetime_from_date_parsing",
        }
    }
}
impl std::fmt::Display for DateTimeQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.source_category())
    }
}
impl std::error::Error for DateTimeQueryError {}

/// Parse a query string with the exact frozen Pydantic Speedate configuration.
///
/// # Errors
/// Returns Pydantic's value-free datetime or datetime-from-date error category.
pub fn parse(value: &str) -> Result<ParsedDateTime, DateTimeQueryError> {
    let config = DateTimeConfig {
        timestamp_unit: TimestampUnit::Infer,
        time_config: TimeConfig {
            microseconds_precision_overflow_behavior:
                MicrosecondsPrecisionOverflowBehavior::Truncate,
            unix_timestamp_offset: Some(0),
        },
    };
    let parsed = if let Ok(datetime) = DateTime::parse_bytes_with_config(value.as_bytes(), &config)
    {
        datetime
    } else {
        let date = Date::parse_bytes_with_config(
            value.as_bytes(),
            &DateConfig {
                timestamp_unit: TimestampUnit::Infer,
            },
        )
        .map_err(|_| DateTimeQueryError::DateTimeFromDateParsing)?;
        DateTime {
            date,
            time: speedate::Time {
                hour: 0,
                minute: 0,
                second: 0,
                microsecond: 0,
                tz_offset: None,
            },
        }
    };
    // Speedate permits year zero; constructing Python's datetime rejects it.
    if parsed.date.year == 0 {
        return Err(DateTimeQueryError::DateTimeParsing);
    }
    let date = NaiveDate::from_ymd_opt(
        i32::from(parsed.date.year),
        u32::from(parsed.date.month),
        u32::from(parsed.date.day),
    )
    .ok_or(DateTimeQueryError::DateTimeParsing)?;
    let local = date
        .and_hms_micro_opt(
            u32::from(parsed.time.hour),
            u32::from(parsed.time.minute),
            u32::from(parsed.time.second),
            parsed.time.microsecond,
        )
        .ok_or(DateTimeQueryError::DateTimeParsing)?;
    if let Some(seconds) = parsed.time.tz_offset {
        let aware = FixedOffset::east_opt(seconds)
            .and_then(|offset| offset.from_local_datetime(&local).single())
            .ok_or(DateTimeQueryError::DateTimeParsing)?;
        Ok(ParsedDateTime::Aware(Timestamp(aware)))
    } else {
        Ok(ParsedDateTime::Naive(local))
    }
}
