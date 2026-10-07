use crate::{Error, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, Timelike, Utc};

fn instant(value: &str) -> Result<DateTime<FixedOffset>> {
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| Error::CorruptData)?;
    parsed
        .with_nanosecond(parsed.nanosecond() / 1000 * 1000)
        .ok_or(Error::CorruptData)
}

fn offset_moment(value: &str) -> Result<DateTime<FixedOffset>> {
    if value.len() == 10 {
        Ok(moment(value)?.fixed_offset())
    } else {
        instant(value)
    }
}
fn offset_stored(value: &str, floor: DateTime<FixedOffset>) -> Result<DateTime<FixedOffset>> {
    let when = offset_moment(value)?;
    // Python max keeps its first argument on equal instants, including its offset.
    Ok(if value.len() == 10 && when < floor {
        floor
    } else {
        when
    })
}
fn evidence_time(value: DateTime<FixedOffset>) -> String {
    value.to_rfc3339_opts(
        if value.timestamp_subsec_micros() == 0 {
            SecondsFormat::Secs
        } else {
            SecondsFormat::Micros
        },
        false,
    )
}

pub(crate) fn moment(value: &str) -> Result<DateTime<Utc>> {
    if value.len() == 10 {
        let day = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| Error::CorruptData)?;
        Ok(day
            .and_hms_opt(0, 0, 0)
            .ok_or(Error::CorruptData)?
            .and_utc())
    } else {
        instant(value).map(|v| v.with_timezone(&Utc))
    }
}
pub(crate) fn before(later: &str, earlier: &str) -> Result<bool> {
    let (later_time, earlier_time) = (moment(later)?, moment(earlier)?);
    Ok(if later.len() == 10 || earlier.len() == 10 {
        later_time.date_naive() < earlier_time.date_naive()
    } else {
        later_time < earlier_time
    })
}
pub(crate) fn stored(value: &str, floor: Option<DateTime<Utc>>) -> Result<DateTime<Utc>> {
    let when = moment(value)?;
    Ok(if value.len() == 10 {
        floor.map_or(when, |floor| when.max(floor))
    } else {
        when
    })
}
pub(crate) struct AttemptTimes {
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    pub evaluated: DateTime<Utc>,
    pub started_text: String,
    pub ended_text: String,
    pub evaluated_text: String,
}
impl AttemptTimes {
    pub fn ended(&self) -> DateTime<Utc> {
        self.finished.unwrap_or(self.started)
    }
}
pub(crate) fn attempts(content: &serde_json::Value) -> Result<Vec<AttemptTimes>> {
    let created = offset_moment(crate::semantic::text(content, "created_at")?)?;
    crate::semantic::items(content, "attempts")
        .iter()
        .map(|attempt| {
            let started = offset_stored(crate::semantic::text(attempt, "started_at")?, created)?;
            let finished = attempt
                .get("finished_at")
                .and_then(serde_json::Value::as_str)
                .map(|v| offset_stored(v, started))
                .transpose()?;
            let ended = finished.unwrap_or(started);
            let evaluated = attempt["verdict"]
                .get("evaluated_at")
                .and_then(serde_json::Value::as_str)
                .map_or(Ok(ended), |v| offset_stored(v, ended))?;
            Ok(AttemptTimes {
                started: started.with_timezone(&Utc),
                finished: finished.map(|value| value.with_timezone(&Utc)),
                evaluated: evaluated.with_timezone(&Utc),
                started_text: evidence_time(started),
                ended_text: evidence_time(ended),
                evaluated_text: evidence_time(evaluated),
            })
        })
        .collect()
}
pub(crate) fn decided(
    content: &serde_json::Value,
    times: &[AttemptTimes],
) -> Result<Option<DateTime<Utc>>> {
    let Some(decision) = content.get("decision") else {
        return Ok(None);
    };
    let floor = if decision["action"] == "decline" || times.is_empty() {
        moment(crate::semantic::text(content, "created_at")?)?
    } else if decision["action"] == "close_failed" {
        times.last().ok_or(Error::CorruptData)?.ended()
    } else {
        times.last().ok_or(Error::CorruptData)?.evaluated
    };
    stored(crate::semantic::text(decision, "decided_at")?, Some(floor)).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{attempts, moment};
    use serde_json::json;

    #[test]
    fn submicrosecond_instants_are_truncated_before_date_floor_comparison() -> crate::Result<()> {
        let times = attempts(
            &json!({"created_at":"2025-02-05T01:00:00.0000009+01:00","attempts":[
                {"started_at":"2025-02-05","finished_at":"2025-02-05T01:00:00.1234569+01:00","verdict":{"evaluated_at":"2025-02-05"}}
            ]}),
        )?;
        assert_eq!(
            moment("2025-02-05T01:00:00.0000009+01:00")?,
            moment("2025-02-05")?
        );
        assert_eq!(times[0].started_text, "2025-02-05T00:00:00+00:00");
        assert_eq!(times[0].started, moment("2025-02-05")?);
        assert_eq!(times[0].ended_text, "2025-02-05T01:00:00.123456+01:00");
        assert_eq!(times[0].evaluated_text, times[0].ended_text);
        assert_eq!(times[0].evaluated, moment("2025-02-05T00:00:00.123456Z")?);
        Ok(())
    }

    #[test]
    fn evidence_preserves_original_offsets_and_microsecond_precision() -> crate::Result<()> {
        let times = attempts(&json!({"created_at":"2025-02-04","attempts":[
            {"started_at":"2025-02-04T09:30:00+01:00","finished_at":"2025-02-04T10:05:00+01:00","verdict":{"evaluated_at":"2025-02-04"}},
            {"started_at":"2025-02-05T09:30:00.1-07:00","finished_at":"2025-02-05T18:45:00.001+02:00","verdict":{"evaluated_at":"2025-02-05T18:46:00.123456+02:00"}}
        ]}))?;
        assert_eq!(times[0].started_text, "2025-02-04T09:30:00+01:00");
        assert_eq!(times[0].ended_text, "2025-02-04T10:05:00+01:00");
        assert_eq!(times[0].evaluated_text, "2025-02-04T10:05:00+01:00");
        assert_eq!(times[0].started, moment("2025-02-04T08:30:00Z")?);
        assert_eq!(times[0].evaluated, moment("2025-02-04T09:05:00Z")?);
        assert_eq!(times[1].started_text, "2025-02-05T09:30:00.100000-07:00");
        assert_eq!(times[1].ended_text, "2025-02-05T18:45:00.001000+02:00");
        assert_eq!(times[1].evaluated_text, "2025-02-05T18:46:00.123456+02:00");
        Ok(())
    }

    #[test]
    fn date_floors_retain_selected_offsets_and_equal_dates_use_midnight_utc() -> crate::Result<()> {
        let times = attempts(
            &json!({"created_at":"2025-02-04T09:30:00.123456+01:00","attempts":[
                {"started_at":"2025-02-04","finished_at":"2025-02-04","verdict":{"evaluated_at":"2025-02-04"}},
                {"started_at":"2025-02-05T01:00:00+01:00","finished_at":"2025-02-05"},
                {"started_at":"2025-02-06T02:00:00-07:00"}
            ]}),
        )?;
        for text in [
            &times[0].started_text,
            &times[0].ended_text,
            &times[0].evaluated_text,
        ] {
            assert_eq!(text, "2025-02-04T09:30:00.123456+01:00");
        }
        assert_eq!(times[1].ended_text, "2025-02-05T00:00:00+00:00");
        assert_eq!(times[1].evaluated_text, times[1].ended_text);
        assert_eq!(times[2].ended_text, times[2].started_text);
        assert_eq!(times[2].evaluated_text, times[2].started_text);
        assert!(times[2].finished.is_none());
        Ok(())
    }
}
