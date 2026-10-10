use crate::{
    Bundle, Entry, Error, Plan, Result, canonical_sha256,
    semantic::{self, items, provenance, text},
    time,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    ids::{AttemptId, HypothesisId, ProjectId, ReviewCaseId, TrackId, UserId},
    principal::{Channel, Via},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, types::Json};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Copy)]
struct EvidenceId(Uuid);
#[derive(Clone, Copy)]
struct FailureId(Uuid);
pub(crate) struct Writer<'a> {
    pub connection: &'a mut PgConnection,
    pub project: ProjectId,
    pub creator: UserId,
    pub users: &'a BTreeMap<String, UserId>,
    pub science: &'a Value,
    pub revision: i32,
    pub bundle: &'a Bundle,
    pub reports: &'a BTreeMap<String, String>,
    pub plan: &'a mut Plan,
    pub tracks: BTreeMap<String, TrackId>,
    pub policies: &'a BTreeMap<String, Value>,
}
impl Writer<'_> {
    pub async fn audit(
        &mut self,
        action: &str,
        subject_type: &str,
        subject_id: &str,
        mut state: Value,
    ) -> Result<()> {
        state["bundle_sha256"] = json!(self.bundle.sha256);
        let via = Via {
            channel: Channel::Cli,
            client: Some("cannery import".into()),
        };
        audit::record(
            self.connection,
            Attribution::System(Some(&via)),
            Record {
                action,
                subject_type,
                subject_id,
                project_id: Some(self.project),
                prior_state: None,
                new_state: Some(&state),
                reason: None,
                idempotency_key: None,
            },
        )
        .await
        .map(|_| ())
        .map_err(|_| Error::Database {
            code: None,
            constraint: None,
        })
    }
    pub async fn entry(&mut self, entry: &Entry) -> Result<()> {
        let digest = canonical_sha256(&entry.content);
        sqlx::query!(
            "INSERT INTO import_entries(project_id,kind,key,content,sha256,bundle_sha256) VALUES($1,$2,$3,$4,$5,$6)",
            self.project.0 as _,
            entry.kind.as_str(),
            entry.key,
            Json(&entry.content) as _,
            digest,
            self.bundle.sha256,
        ).execute(&mut *self.connection).await?;
        Ok(())
    }
    pub async fn policy(&mut self, entry: &Entry) -> Result<()> {
        let value = &entry.content;
        sqlx::query!(
            "INSERT INTO historical_policies(project_id,id,revision,content,source_ref) VALUES($1,$2,$3,$4,$5)",
            self.project.0 as _,
            text(value,"id")?,
            text(value,"revision")?,
            Json(value) as _,
            text(value,"source")?,
        ).execute(&mut *self.connection).await?;
        self.plan.policies += 1;
        self.audit(
            "import.policy",
            "historical_policy",
            &format!(
                "{}:{}:{}",
                self.project,
                text(value, "id")?,
                text(value, "revision")?
            ),
            json!({"id":value["id"],"revision":value["revision"]}),
        )
        .await
    }
    pub async fn track(&mut self, entry: &Entry) -> Result<()> {
        let value = &entry.content;
        let created = time::moment(text(value, "created_at")?)?;
        let updated = value
            .get("archived_at")
            .and_then(Value::as_str)
            .map_or(Ok(created), |v| time::stored(v, Some(created)))?;
        let id=TrackId(sqlx::query_scalar!(
            "INSERT INTO tracks(project_id,slug,title,description,state,created_by,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id AS \"id: uuid::Uuid\"",
            self.project.0 as _,
            text(value,"slug")?,
            text(value,"title")?,
            value["description"].as_str().unwrap_or(""),
            text(value,"state")?,
            self.creator.0 as _,
            created as _,
            updated as _,
        ).fetch_one(&mut *self.connection).await?);
        self.tracks.insert(entry.key.clone(), id);
        self.plan.tracks += 1;
        self.audit(
            "import.track",
            "track",
            &id.to_string(),
            json!({"slug":value["slug"],"state":value["state"]}),
        )
        .await
    }
    pub async fn hypothesis(
        &mut self,
        entry: &Entry,
        ids: &mut BTreeMap<String, HypothesisId>,
        numbers: &mut BTreeMap<String, i32>,
    ) -> Result<()> {
        let value = &entry.content;
        let state = text(value, "state")?;
        let created = time::moment(text(value, "created_at")?)?;
        let times = time::attempts(value)?;
        let decision = time::decided(value, &times)?;
        let updated = times
            .iter()
            .map(|time| time.evaluated)
            .chain(decision)
            .fold(created, std::cmp::max);
        let approved_revision = Some(1i32);
        let approved_at = Some(created);
        let track = self
            .tracks
            .get(text(value, "track")?)
            .ok_or(Error::CorruptData)?;
        let imported = provenance(
            value,
            &["id", "kind", "claim", "control", "sources", "notes"],
        );
        let number=sqlx::query_scalar!(
            "UPDATE projects SET next_hypothesis_number=next_hypothesis_number+1 WHERE id=$1 RETURNING next_hypothesis_number-1 AS \"number!\"",
            self.project.0 as _,
        ).fetch_one(&mut *self.connection).await?;
        let id=HypothesisId(sqlx::query_scalar!(
            "INSERT INTO hypotheses(project_id,number,track_id,state,revision,approved_revision,title,created_by_user,created_at,updated_at,approved_at,origin,source_ref,external_id,imported) VALUES($1,$2,$3,$4,1,$5,$6,$7,$8,$9,$10,'imported',$11,$12,$13) RETURNING id AS \"id: uuid::Uuid\"",
            self.project.0 as _,
            number,
            track.0 as _,
            state,
            approved_revision,
            text(value,"title")?,
            self.creator.0 as _,
            created as _,
            updated as _,
            approved_at as _,
            entry.path,
            entry.key,
            Json(&imported) as _,
        ).fetch_one(&mut *self.connection).await?);
        ids.insert(entry.key.clone(), id);
        numbers.insert(entry.key.clone(), number);
        Ok(())
    }
    #[allow(clippy::too_many_lines)] // Ordered revision/run/review creation shares the historical identity maps.
    pub async fn history(
        &mut self,
        entry: &Entry,
        ids: &BTreeMap<String, HypothesisId>,
        numbers: &BTreeMap<String, i32>,
    ) -> Result<()> {
        let value = &entry.content;
        let id = *ids.get(&entry.key).ok_or(Error::CorruptData)?;
        let content = semantic::imported_document(value, numbers);
        let created = time::moment(text(value, "created_at")?)?;
        sqlx::query!(
            "INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel,via_client,created_at,origin,source_ref) VALUES($1,1,$2,$3,$4,'cli','cannery import',$5,'imported',$6)",
            id.0 as _,
            Json(&content) as _,
            self.revision,
            self.creator.0 as _,
            created as _,
            entry.path,
        ).execute(&mut *self.connection).await?;
        for relation in items(value, "relations") {
            let target = ids.get(text(relation, "to")?).ok_or(Error::CorruptData)?;
            sqlx::query!(
                "INSERT INTO hypothesis_relations(hypothesis_id,kind,target_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
                id.0 as _,
                text(relation,"type")?,
                target.0 as _,
            ).execute(&mut *self.connection).await?;
        }
        let state = text(value, "state")?;
        let decision = value.get("decision");
        let attempts = items(value, "attempts");
        let times = time::attempts(value)?;
        let decided = time::decided(value, &times)?;
        let mut previous: Option<AttemptId> = None;
        for (index, (attempt, times)) in attempts.iter().zip(&times).enumerate() {
            let last = index + 1 == attempts.len();
            let has_decision = last && decision.is_some();
            let attempt_state = if last && state == "awaiting_human_review" || has_decision {
                state
            } else if attempt["status"] == "failed" {
                "failed"
            } else {
                "unreviewed"
            };
            let source = attempt.get("source").and_then(Value::as_str).map_or_else(
                || format!("{}#/attempts/{index}", entry.path),
                str::to_owned,
            );
            let imported = provenance(attempt, &["label", "config", "notes", "source_revision"]);
            let track = self
                .tracks
                .get(text(value, "track")?)
                .ok_or(Error::CorruptData)?;
            let sequence = i32::try_from(index + 1)
                .map_err(|_| Error::problem(&entry.path, "/attempts", "too many attempts"))?;
            let previous_id = previous.map(|id| id.0);
            let attempt_id=AttemptId(sqlx::query_scalar!(
                "INSERT INTO attempts(project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,via_client,predecessor_id,lease_generation,claimed_at,started_at,finished_at,origin,source_ref,imported) VALUES($1,$2,$3,$4,1,$5,$6,$7,'cli','cannery import',$8,0,$9,$9,$10,'imported',$11,$12) RETURNING id AS \"id: uuid::Uuid\"",
                self.project.0 as _,
                id.0 as _,
                sequence,
                attempt_state,
                self.revision,
                track.0 as _,
                self.creator.0 as _,
                previous_id as _,
                times.started as _,
                times.finished as _,
                source,
                Json(&imported) as _,
            ).fetch_one(&mut *self.connection).await?);
            previous = Some(attempt_id);
            self.plan.attempts += 1;
            self.attempt_records(
                entry,
                index,
                id,
                attempt_id,
                attempt,
                times,
                attempt_state,
                if has_decision { decision } else { None },
                decided,
            )
            .await?;
        }
        self.plan.hypotheses += 1;
        *self
            .plan
            .by_track
            .entry(text(value, "track")?.into())
            .or_default() += 1;
        *self.plan.by_state.entry(state.into()).or_default() += 1;
        self.audit(
            "import.hypothesis",
            "hypothesis",
            &id.to_string(),
            json!({"number":numbers.get(&entry.key),"external_id":entry.key,"state":state}),
        )
        .await
    }
    async fn report(&mut self, attempt: AttemptId, report: &Value) -> Result<()> {
        let path = text(report, "path")?;
        let written = text(report, "written_at")?;
        let day = if written.len() == 10 {
            Some(NaiveDate::parse_from_str(written, "%Y-%m-%d").map_err(|_| Error::CorruptData)?)
        } else {
            None
        };
        let instant = if day.is_none() {
            Some(time::moment(written)?)
        } else {
            None
        };
        let body = self.reports.get(path).ok_or(Error::CorruptData)?;
        sqlx::query!(
            "INSERT INTO phase_outputs(project_id,attempt_id,stage,status,revision,front_matter,body,sha256,via_channel,via_client,origin,source_ref) VALUES($1,$2,'writeup','completed',1,jsonb_strip_nulls(jsonb_build_object('kind',$3::text,'author',$4::text,'written_on',$5::date,'written_at',$6::timestamptz)),$7,$8,'cli','cannery import','imported',$9)",
            self.project.0 as _,
            attempt.0 as _,
            text(report,"kind")?,
            text(report,"author")?,
            day as _,
            instant as _,
            body,
            text(report,"sha256")?,
            path,
        ).execute(&mut *self.connection).await?;
        self.plan.reports += 1;
        Ok(())
    }
    async fn verification(
        &mut self,
        attempt: AttemptId,
        front_matter: &Value,
        body: &str,
        created: DateTime<Utc>,
        source: &str,
    ) -> Result<EvidenceId> {
        let digest = canonical_sha256(&json!({"front_matter":front_matter,"body":body}));
        Ok(EvidenceId(sqlx::query_scalar!(
            "INSERT INTO phase_outputs(project_id,attempt_id,stage,status,revision,front_matter,body,sha256,via_channel,via_client,created_at,origin,source_ref) VALUES($1,$2,'verification','completed',1,$3,$4,$5,'cli','cannery import',$6,'imported',$7) RETURNING id AS \"id: uuid::Uuid\"",
            self.project.0 as _,
            attempt.0 as _,
            Json(front_matter) as _,
            body,
            digest,
            created as _,
            source,
        ).fetch_one(&mut *self.connection).await?))
    }
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One historical run owns its related immutable records.
    async fn attempt_records(
        &mut self,
        entry: &Entry,
        index: usize,
        hypothesis: HypothesisId,
        attempt: AttemptId,
        value: &Value,
        times: &time::AttemptTimes,
        state: &str,
        decision: Option<&Value>,
        decided: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let path = format!("{}#/attempts/{index}", entry.path);
        for (index, artifact) in items(value, "artifacts").iter().enumerate() {
            let source = format!("{path}/artifacts/{index}");
            let size = artifact["size"].as_i64().ok_or_else(|| {
                Error::problem(
                    &entry.path,
                    "/attempts/artifacts/size",
                    "size exceeds the database range",
                )
            })?;
            sqlx::query!(
                "INSERT INTO artifacts(project_id,attempt_id,role,backend,bucket,key,uri,size_bytes,sha256,media_type,origin,source_ref) VALUES($1,$2,$3,'external','',$4,$4,$5,$6,$7,'imported',$8)",
                self.project.0 as _,
                attempt.0 as _,
                text(artifact,"role")?,
                text(artifact,"uri")?,
                size,
                text(artifact,"sha256")?,
                artifact["media_type"].as_str().unwrap_or("application/octet-stream"),
                source,
            ).execute(&mut *self.connection).await?;
        }
        if items(value, "artifacts").is_empty() {
            self.gap("attempts without an artifact");
        }
        if let Some(report) = value.get("report") {
            self.report(attempt, report).await?;
        }
        let mut provenance = json!({"science_revision":self.revision.to_string()});
        if let Some(revision) = value.get("source_revision") {
            provenance["source_revision"] = revision.clone();
        }
        let measurements = items(value, "measurements");
        if measurements.is_empty() {
            self.gap("attempts without a measurement");
        }
        let mut stored = Vec::new();
        for measurement in measurements {
            let metric = items(self.science, "metrics")
                .iter()
                .find(|metric| metric["key"] == measurement["metric"])
                .ok_or(Error::CorruptData)?;
            let mut kept = semantic::provenance(
                measurement,
                &[
                    "metric",
                    "value",
                    "missing_reason",
                    "authority",
                    "split",
                    "sample_count",
                    "control_value",
                    "uncertainty",
                    "source",
                ],
            );
            kept["unit"] = metric["unit"].clone();
            kept["direction"] = metric["direction"].clone();
            kept["dimensions"] = measurement
                .get("dimensions")
                .cloned()
                .unwrap_or_else(|| json!({}));
            stored.push(kept);
            *self
                .plan
                .by_authority
                .entry(text(measurement, "authority")?.into())
                .or_default() += 1;
            if measurement.get("missing_reason").is_some() {
                self.gap("measurements with a missing value");
            }
        }
        let failure = if value["status"] == "failed" {
            let failure = &value["failure"];
            Some(FailureId(sqlx::query_scalar!(
                "INSERT INTO attempt_failures(attempt_id,stage,code,reason,created_at) VALUES($1,$2,$3,$4,$5) RETURNING id AS \"id: uuid::Uuid\"",
                attempt.0 as _,
                failure["stage"].as_str().unwrap_or("agent"),
                text(failure,"code")?,
                text(failure,"reason")?,
                times.ended() as _,
            ).fetch_one(&mut *self.connection).await?))
        } else {
            None
        };
        let verdict = value.get("verdict");
        if verdict.is_none() && value["status"] == "completed" {
            self.gap("completed attempts without a verdict");
        }
        // One verification report holds the historical verdict and the measurements it rests on.
        let verification = if verdict.is_some() || !stored.is_empty() {
            let mut front_matter = json!({"measurements":stored,"provenance":provenance});
            let (created, source) = if let Some(verdict) = verdict {
                let policy = text(verdict, "policy")?;
                let policy_revision = text(
                    self.policies.get(policy).ok_or(Error::CorruptData)?,
                    "revision",
                )?;
                front_matter["verdict"] = verdict["result"].clone();
                front_matter["reason"] = verdict["reason"].clone();
                front_matter["policy_revision"] = json!(format!("{policy}@{policy_revision}"));
                front_matter["gates"] = verdict["gates"].clone();
                if let Some(comparisons) = verdict.get("comparisons") {
                    front_matter["comparisons"] = comparisons.clone();
                }
                (times.evaluated, String::from(text(verdict, "source")?))
            } else {
                (times.ended(), format!("{path}/measurements"))
            };
            let body = notes(value);
            Some(
                self.verification(attempt, &front_matter, body, created, &source)
                    .await?,
            )
        } else {
            None
        };
        if state == "awaiting_human_review" {
            self.case(Case {
                hypothesis,
                attempt: Some(attempt),
                kind: "result",
                opened: times.evaluated,
                resolved: None,
                decision: None,
                evidence: verification.filter(|_| verdict.is_some()),
                failure: None,
                source: text(verdict.ok_or(Error::CorruptData)?, "source")?,
            })
            .await?;
        } else if let Some(decision) = decision {
            let failed = decision["action"] == "close_failed";
            self.case(Case {
                hypothesis,
                attempt: Some(attempt),
                kind: if failed { "failure" } else { "result" },
                opened: if failed {
                    times.ended()
                } else {
                    times.evaluated
                },
                resolved: decided,
                decision: Some(decision),
                evidence: verification.filter(|_| verdict.is_some()),
                failure,
                source: text(decision, "source")?,
            })
            .await?;
        }
        Ok(())
    }
    fn gap(&mut self, key: &str) {
        *self.plan.gaps.entry(key.into()).or_default() += 1;
    }
    async fn case(&mut self, case: Case<'_>) -> Result<()> {
        let attempt = case.attempt.map(|id| id.0);
        let evidence = case.evidence.map(|id| id.0);
        let failure = case.failure.map(|id| id.0);
        let state = if case.decision.is_some() {
            "resolved"
        } else {
            "pending"
        };
        let id=ReviewCaseId(sqlx::query_scalar!(
            "INSERT INTO review_cases(project_id,hypothesis_id,attempt_id,kind,subject_revision,state,opened_at,resolved_at,evidence_id,failure_id,origin,source_ref) VALUES($1,$2,$3,$4,1,$5,$6,$7,$8,$9,'imported',$10) RETURNING id AS \"id: uuid::Uuid\"",
            self.project.0 as _,
            case.hypothesis.0 as _,
            attempt as _,
            case.kind,
            state,
            case.opened as _,
            case.resolved as _,
            evidence as _,
            failure as _,
            case.source,
        ).fetch_one(&mut *self.connection).await?);
        if let Some(decision) = case.decision {
            let actor = self
                .users
                .get(text(decision, "decided_by")?)
                .ok_or(Error::CorruptData)?;
            sqlx::query!(
                "INSERT INTO decisions(review_case_id,action,subject_revision,reason,actor_user_id,via_channel,via_client,decided_at,origin,source_ref) VALUES($1,$2,1,$3,$4,'cli','cannery import',$5,'imported',$6)",
                id.0 as _,
                text(decision,"action")?,
                text(decision,"reason")?,
                actor.0 as _,
                case.resolved as _,
                text(decision,"source")?,
            ).execute(&mut *self.connection).await?;
            self.plan.decisions += 1;
        }
        Ok(())
    }
}
struct Case<'a> {
    hypothesis: HypothesisId,
    attempt: Option<AttemptId>,
    kind: &'a str,
    opened: DateTime<Utc>,
    resolved: Option<DateTime<Utc>>,
    decision: Option<&'a Value>,
    evidence: Option<EvidenceId>,
    failure: Option<FailureId>,
    source: &'a str,
}

/// The attempt's notes become the verification report's body, exactly as written.
fn notes(attempt: &Value) -> &str {
    attempt.get("notes").and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::notes;
    use crate::canonical_sha256;
    use serde_json::json;

    #[test]
    fn notes_are_the_body_and_count_in_the_digest_but_whitespace_is_retained() {
        let front_matter = json!({"measurements":[{"value":0.71}]});
        let digest =
            |body: &str| canonical_sha256(&json!({"front_matter":front_matter,"body":body}));
        assert_eq!(notes(&json!({})), "");
        assert_eq!(notes(&json!({"notes":""})), "");
        assert_eq!(
            digest(notes(&json!({}))),
            digest(notes(&json!({"notes":""})))
        );
        assert_eq!(notes(&json!({"notes":" \n"})), " \n");
        assert_ne!(digest(" \n"), digest(""));
    }
}
