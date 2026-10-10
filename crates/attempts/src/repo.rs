//! Explicit-connection persistence; callers own transactions and permissions.
#[derive(Debug, thiserror::Error)]
pub enum AttemptError {
    #[error("attempt database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("attempt persistence invariant failed")]
    Invariant,
    #[error("attempt stored domain value is invalid")]
    CorruptData,
    #[error("attempt no longer holds the expected lease")]
    StaleLease,
    #[error("attempt persistence conflict")]
    Conflict,
    #[error("source JSON recursion limit exceeded")]
    SourceRecursion,
    #[error("source JSON integer rendering limit exceeded")]
    SourceIntegerLimit,
    #[error("source text encoding failed")]
    Encoding,
    #[error("source text contains NUL")]
    TextNul,
    #[error("source stored control key is absent")]
    ControlKey,
    #[error("source stored control shape is invalid")]
    ControlShape,
}
impl AttemptError {
    pub(crate) fn database(error: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|code| {
                    code.len() == 5
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}
use crate::{
    arguments::{Argument, JsonParameter, Value},
    model::{
        Artifact, ArtifactId, Attempt, EvidenceId, Failure, JsonContext, Manifest, ManifestId,
        RefusedUpload, State, StoredJson, TrackPin, Upload, UploadId, actor,
    },
    raw::{RawArtifact, RawFailure, RawManifest, RawRefusedUpload, RawTrackPin, RawUpload},
    statements::{self, Statement},
    wire::{self, JsonbText},
};
use cannery_core::{
    ids::{AttemptId, JobId, ProjectId, ReviewCaseId, UnitId},
    json::{self, Document, Node},
    principal::{Channel, Principal},
    text,
};
use num_bigint::BigInt;
use sqlx::{FromRow, PgConnection, Row, postgres::PgRow};
use std::sync::Arc;
use uuid::Uuid;
/// A borrowed physical connection and its caller-owned preparation state.
/// The repository neither begins transactions nor performs authorization.
pub struct Repository<'a> {
    conn: &'a mut PgConnection,
    context: JsonContext,
}
impl<'a> Repository<'a> {
    #[must_use]
    pub fn new(conn: &'a mut PgConnection, context: JsonContext) -> Self {
        Self { conn, context }
    }
    async fn execute(
        &mut self,
        statement: Statement,
        values: Vec<Value>,
    ) -> Result<(u64, Vec<PgRow>), AttemptError> {
        statements::execute(self.conn, statement, values).await
    }
    async fn rows<T>(
        &mut self,
        statement: Statement,
        values: Vec<Value>,
    ) -> Result<Vec<T>, AttemptError>
    where
        for<'r> T: FromRow<'r, PgRow>,
    {
        self.execute(statement, values)
            .await?
            .1
            .iter()
            .map(|row| T::from_row(row).map_err(|error| AttemptError::database(&error)))
            .collect()
    }
    async fn uuid(
        &mut self,
        statement: Statement,
        values: Vec<Value>,
    ) -> Result<Option<Uuid>, AttemptError> {
        self.execute(statement, values)
            .await?
            .1
            .first()
            .map(|row| {
                row.try_get::<Uuid, _>(0)
                    .map_err(|error| AttemptError::database(&error))
            })
            .transpose()
    }
    async fn count(
        &mut self,
        statement: Statement,
        values: Vec<Value>,
    ) -> Result<i64, AttemptError> {
        self.execute(statement, values)
            .await?
            .1
            .first()
            .ok_or(AttemptError::Invariant)?
            .try_get(0)
            .map_err(|error| AttemptError::database(&error))
    }
    fn json(&self, document: &Document) -> Result<Value, AttemptError> {
        {
            let mut builder = json::DocumentBuilder::new();
            let root = builder
                .import(document, document.root())
                .map_err(|_| AttemptError::CorruptData)?;
            let document = builder
                .finish(root)
                .map_err(|_| AttemptError::CorruptData)?;
            Ok(Value::Json(Some(JsonParameter {
                document: Arc::new(document),
                context: self.context,
            })))
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn pick_claimable(
        &mut self,
        project_id: ProjectId,
        number: Option<&BigInt>,
        track_slug: Option<&str>,
        mode: &str,
        skip_tracks: &[String],
    ) -> Result<Option<UnitId>, AttemptError> {
        self.uuid(
            Statement::PickClaimable0,
            vec![
                Value::uuid(project_id.0),
                Value::text(Some(mode)),
                Value::integer(number),
                Value::integer(number),
                Value::text(track_slug),
                Value::text(track_slug),
                Value::TextList(Some(skip_tracks.to_vec())),
            ],
        )
        .await
        .map(|value| value.map(UnitId))
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn create_attempt(
        &mut self,
        input: CreateAttempt<'_>,
    ) -> Result<AttemptId, AttemptError> {
        let (user, service) = actor(input.principal);
        let values = vec![
            Value::uuid(input.unit_id.0),
            Value::integer(Some(input.science_revision)),
            self.json(input.producer)?,
            Value::Uuid(user.map(|id| id.0)),
            Value::Uuid(service.map(|id| id.0)),
            Value::text(Some(channel(input.principal.via().channel))),
            Value::text(input.principal.via().client.as_deref()),
            Value::Bytes(Some(input.token_hash.to_vec())),
            Value::integer(Some(input.ttl_seconds)),
            input
                .workflow
                .filter(|value| !matches!(value.node(value.root()), Some(Node::Null)))
                .map(|value| self.json(value))
                .transpose()?
                .unwrap_or(Value::Json(None)),
            Value::integer(input.deadline_seconds),
        ];
        self.uuid(Statement::CreateAttempt0, values)
            .await?
            .map(AttemptId)
            .ok_or(AttemptError::Invariant)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_attempt(
        &mut self,
        project: ProjectId,
        number: &BigInt,
        sequence: &BigInt,
        lock: bool,
    ) -> Result<Option<Attempt>, AttemptError> {
        let values = vec![
            Value::uuid(project.0),
            Value::integer(Some(number)),
            Value::integer(Some(sequence)),
        ];
        if lock {
            let id = self.uuid(Statement::GetAttempt0, values).await?;
            match id {
                Some(id) => self.get_attempt_by_id(AttemptId(id), false).await,
                None => Ok(None),
            }
        } else {
            let context = self.context;
            let args = values
                .iter()
                .map(Argument::new)
                .collect::<Result<Vec<_>, _>>()?;
            statements::get_attempt_query(&args)
                .fetch_optional(&mut *self.conn)
                .await
                .map_err(|error| AttemptError::database(&error))?
                .map(|value| value.decode(context))
                .transpose()
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_attempt_by_id(
        &mut self,
        id: AttemptId,
        lock: bool,
    ) -> Result<Option<Attempt>, AttemptError> {
        if lock {
            self.execute(Statement::GetAttemptById0, vec![Value::uuid(id.0)])
                .await?;
        }
        let context = self.context;
        let values = [Value::uuid(id.0)];
        let args = values
            .iter()
            .map(Argument::new)
            .collect::<Result<Vec<_>, _>>()?;
        statements::get_attempt_by_id_query(&args)
            .fetch_optional(&mut *self.conn)
            .await
            .map_err(|error| AttemptError::database(&error))?
            .map(|value| value.decode(context))
            .transpose()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_attempts(
        &mut self,
        unit: UnitId,
        after: Option<&BigInt>,
        limit: Option<&BigInt>,
    ) -> Result<Vec<Attempt>, AttemptError> {
        let context = self.context;
        let values = [
            Value::uuid(unit.0),
            Value::integer(after),
            Value::integer(after),
            Value::integer(limit),
        ];
        let args = values
            .iter()
            .map(Argument::new)
            .collect::<Result<Vec<_>, _>>()?;
        statements::list_attempts_query(&args)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(|error| AttemptError::database(&error))?
            .into_iter()
            .map(|value| value.decode(context))
            .collect()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_project_attempts(
        &mut self,
        project: ProjectId,
        states: Option<&[String]>,
        track_slug: Option<&str>,
        before: Option<AttemptId>,
        limit: &BigInt,
    ) -> Result<Vec<Attempt>, AttemptError> {
        let context = self.context;
        let values = [
            Value::uuid(project.0),
            Value::TextList(states.map(<[String]>::to_vec)),
            Value::text(track_slug),
            Value::Uuid(before.map(|id| id.0)),
            Value::integer(Some(limit)),
        ];
        let args = values
            .iter()
            .map(Argument::new)
            .collect::<Result<Vec<_>, _>>()?;
        statements::list_project_attempts_query(&args)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(|error| AttemptError::database(&error))?
            .into_iter()
            .map(|value| value.decode(context))
            .collect()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn extend_lease(&mut self, id: AttemptId, ttl: &BigInt) -> Result<(), AttemptError> {
        self.execute(
            Statement::ExtendLease0,
            vec![Value::integer(Some(ttl)), Value::uuid(id.0)],
        )
        .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn mark_running(&mut self, id: AttemptId) -> Result<(), AttemptError> {
        self.execute(Statement::MarkRunning0, vec![Value::uuid(id.0)])
            .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn end_lease(&mut self, id: AttemptId, state: &str) -> Result<(), AttemptError> {
        let (count, _) = self
            .execute(
                Statement::EndLease0,
                vec![
                    Value::text(Some(state)),
                    Value::text(Some(state)),
                    Value::text(Some(state)),
                    Value::uuid(id.0),
                ],
            )
            .await?;
        if count == 1 {
            Ok(())
        } else {
            Err(AttemptError::StaleLease)
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn move_attempt(
        &mut self,
        id: AttemptId,
        from_state: &str,
        to_state: &str,
    ) -> Result<(), AttemptError> {
        let (count, _) = self
            .execute(
                Statement::MoveAttempt0,
                vec![
                    Value::text(Some(to_state)),
                    Value::text(Some(to_state)),
                    Value::uuid(id.0),
                    Value::text(Some(from_state)),
                ],
            )
            .await?;
        if count == 1 {
            Ok(())
        } else {
            Err(AttemptError::Conflict)
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn reopen_attempt(
        &mut self,
        id: AttemptId,
        to_state: &str,
    ) -> Result<(), AttemptError> {
        let (count, _) = self
            .execute(
                Statement::ReopenAttempt0,
                vec![Value::text(Some(to_state)), Value::uuid(id.0)],
            )
            .await?;
        if count == 1 {
            Ok(())
        } else {
            Err(AttemptError::Conflict)
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn pin_track(&mut self, unit: UnitId) -> Result<TrackPin, AttemptError> {
        let context = self.context;
        self.rows::<RawTrackPin>(Statement::PinTrack0, vec![Value::uuid(unit.0)])
            .await?
            .into_iter()
            .next()
            .ok_or(AttemptError::Invariant)?
            .decode(context)
    }
    async fn approved_json(
        &mut self,
        statement: Statement,
        unit: UnitId,
    ) -> Result<StoredJson, AttemptError> {
        let (_, rows) = self.execute(statement, vec![Value::uuid(unit.0)]).await?;
        let value: Option<JsonbText> = rows
            .first()
            .ok_or(AttemptError::Invariant)?
            .try_get(0)
            .map_err(|error| AttemptError::database(&error))?;
        wire::stored(value.as_ref().map(|value| value.0.as_str()), self.context)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn approved_project_fields(
        &mut self,
        unit: UnitId,
    ) -> Result<StoredJson, AttemptError> {
        let value = self
            .approved_json(Statement::ApprovedProjectFields0, unit)
            .await?;
        if truthy(&value) {
            Ok(value)
        } else {
            Ok(StoredJson::Value(Arc::new(
                json::decode(b"{}", self.context.decode_nesting_budget)
                    .map_err(|_| AttemptError::CorruptData)?,
            )))
        }
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn approved_control(
        &mut self,
        unit: UnitId,
        render_nesting_budget: usize,
    ) -> Result<Option<Control>, AttemptError> {
        let value = self
            .approved_json(Statement::ApprovedControl0, unit)
            .await?;
        if value.python_none() {
            return Ok(None);
        }
        let StoredJson::Value(value) = value else {
            return Err(AttemptError::Invariant);
        };
        if !matches!(value.node(value.root()), Some(Node::Object(_))) {
            return Err(AttemptError::ControlShape);
        }
        let render = |key: &str| {
            let id = value
                .field(value.root(), key)
                .ok_or(AttemptError::ControlKey)?;
            text::str_value(&value, id, render_nesting_budget).map_err(|error| match error {
                text::RenderError::IntegerLimit => AttemptError::SourceIntegerLimit,
                text::RenderError::Recursion => AttemptError::SourceRecursion,
                _ => AttemptError::CorruptData,
            })
        };
        let id = render("id")?;
        let revision = render("revision")?;
        Ok(Some(Control { id, revision }))
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn create_upload(
        &mut self,
        input: CreateUpload<'_>,
    ) -> Result<Option<Upload>, AttemptError> {
        let slot = input.slot.unwrap_or(input.key);
        self.execute(
            Statement::CreateUpload0,
            vec![
                Value::text(Some(input.backend)),
                Value::text(Some(input.bucket)),
                Value::text(Some(slot)),
                Value::integer(Some(input.max_stream_seconds)),
            ],
        )
        .await?;
        let values = vec![
            Value::uuid(input.attempt_id.0),
            Value::Uuid(input.job_id.map(|id| id.0)),
            Value::integer(Some(input.lease_generation)),
            Value::Bytes(Some(input.token_hash.to_vec())),
            Value::text(Some(input.role)),
            Value::text(Some(input.backend)),
            Value::text(Some(input.bucket)),
            Value::text(Some(input.key)),
            Value::text(Some(slot)),
            Value::integer(Some(input.declared_size)),
            Value::text(Some(input.declared_sha256)),
            Value::text(Some(input.media_type)),
            Value::text(input.interface),
            Value::text(Some(input.transfer)),
            Value::text(input.multipart_upload_id),
            Value::integer(input.part_size),
            Value::integer(Some(input.ttl_minutes)),
            Value::text(Some(input.backend)),
            Value::text(Some(input.bucket)),
            Value::text(Some(slot)),
            Value::text(Some(input.key)),
            Value::integer(Some(input.max_stream_seconds)),
        ];
        let context = self.context;
        self.rows::<RawUpload>(Statement::CreateUpload1, values)
            .await?
            .into_iter()
            .next()
            .map(|value| value.decode(context))
            .transpose()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn count_open_uploads(
        &mut self,
        attempt: AttemptId,
        job: Option<JobId>,
    ) -> Result<i64, AttemptError> {
        self.count(
            Statement::CountOpenUploads0,
            vec![Value::uuid(attempt.0), Value::Uuid(job.map(|id| id.0))],
        )
        .await
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_upload(
        &mut self,
        id: UploadId,
        lock: bool,
    ) -> Result<Option<Upload>, AttemptError> {
        let context = self.context;
        self.rows::<RawUpload>(
            if lock {
                Statement::GetUpload0Locked
            } else {
                Statement::GetUpload0
            },
            vec![Value::uuid(id.0)],
        )
        .await?
        .into_iter()
        .next()
        .map(|value| value.decode(context))
        .transpose()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn begin_receiving(&mut self, id: UploadId) -> Result<bool, AttemptError> {
        Ok(self
            .execute(Statement::BeginReceiving0, vec![Value::uuid(id.0)])
            .await?
            .0
            == 1)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn abandon_receiving(&mut self, id: UploadId) -> Result<(), AttemptError> {
        self.execute(Statement::AbandonReceiving0, vec![Value::uuid(id.0)])
            .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn finish_upload(
        &mut self,
        id: UploadId,
        state: &str,
        refusal: Option<&Document>,
        object_pending_delete: bool,
    ) -> Result<(), AttemptError> {
        let values = vec![
            Value::text(Some(state)),
            refusal
                .filter(|value| !matches!(value.node(value.root()), Some(Node::Null)))
                .map(|value| self.json(value))
                .transpose()?
                .unwrap_or(Value::Json(None)),
            Value::Bool(Some(object_pending_delete)),
            Value::uuid(id.0),
        ];
        self.execute(Statement::FinishUpload0, values).await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn object_deleted(&mut self, id: UploadId) -> Result<(), AttemptError> {
        self.execute(Statement::ObjectDeleted0, vec![Value::uuid(id.0)])
            .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn record_urls(
        &mut self,
        id: UploadId,
        expires_in: &BigInt,
    ) -> Result<(), AttemptError> {
        self.execute(
            Statement::RecordUrls0,
            vec![Value::integer(Some(expires_in)), Value::uuid(id.0)],
        )
        .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_refused_uploads(
        &mut self,
        job: JobId,
    ) -> Result<Vec<RefusedUpload>, AttemptError> {
        let context = self.context;
        self.rows::<RawRefusedUpload>(Statement::ListRefusedUploads0, vec![Value::uuid(job.0)])
            .await?
            .into_iter()
            .map(|value| value.decode(context))
            .collect()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn add_artifact(&mut self, input: AddArtifact<'_>) -> Result<Artifact, AttemptError> {
        let upload = input.upload;
        let values = vec![
            Value::uuid(input.project_id.0),
            Value::uuid(upload.attempt_id.0),
            Value::Uuid(upload.job_id.map(|id| id.0)),
            Value::text(Some(&upload.role)),
            Value::text(Some(&upload.backend)),
            Value::text(Some(&upload.bucket)),
            Value::text(Some(&upload.key)),
            Value::text(input.generation),
            Value::integer(Some(input.size_bytes)),
            Value::text(Some(input.sha256)),
            Value::text(Some(&upload.media_type)),
            Value::text(upload.interface.as_deref()),
            Value::Bool(if upload.interface.is_some() {
                input.content_validated
            } else {
                None
            }),
        ];
        let context = self.context;
        self.rows::<RawArtifact>(Statement::AddArtifact0, values)
            .await?
            .into_iter()
            .next()
            .ok_or(AttemptError::Invariant)?
            .decode(context)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_artifacts(
        &mut self,
        attempt: AttemptId,
    ) -> Result<Vec<Artifact>, AttemptError> {
        self.artifacts(Statement::ListArtifacts0, vec![Value::uuid(attempt.0)])
            .await
    }
    async fn artifacts(
        &mut self,
        statement: Statement,
        values: Vec<Value>,
    ) -> Result<Vec<Artifact>, AttemptError> {
        let context = self.context;
        self.rows::<RawArtifact>(statement, values)
            .await?
            .into_iter()
            .map(|value| value.decode(context))
            .collect()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_artifact(
        &mut self,
        project: ProjectId,
        id: ArtifactId,
    ) -> Result<Option<Artifact>, AttemptError> {
        Ok(self
            .artifacts(
                Statement::GetArtifact0,
                vec![Value::uuid(id.0), Value::uuid(project.0)],
            )
            .await?
            .into_iter()
            .next())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_artifact_by_key(
        &mut self,
        project: ProjectId,
        backend: &str,
        bucket: &str,
        key: &str,
    ) -> Result<Option<Artifact>, AttemptError> {
        Ok(self
            .artifacts(
                Statement::GetArtifactByKey0,
                vec![
                    Value::text(Some(backend)),
                    Value::text(Some(bucket)),
                    Value::text(Some(key)),
                    Value::uuid(project.0),
                ],
            )
            .await?
            .into_iter()
            .next())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_artifacts_by_role(
        &mut self,
        attempt: AttemptId,
        role: &str,
    ) -> Result<Vec<Artifact>, AttemptError> {
        self.artifacts(
            Statement::ListArtifactsByRole0,
            vec![Value::uuid(attempt.0), Value::text(Some(role))],
        )
        .await
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_job_artifacts(&mut self, job: JobId) -> Result<Vec<Artifact>, AttemptError> {
        self.artifacts(Statement::ListJobArtifacts0, vec![Value::uuid(job.0)])
            .await
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn add_manifest(
        &mut self,
        attempt: AttemptId,
        stage: &str,
        content: &Document,
        sha256: &str,
    ) -> Result<Manifest, AttemptError> {
        let values = vec![
            Value::uuid(attempt.0),
            Value::text(Some(stage)),
            self.json(content)?,
            Value::text(Some(sha256)),
        ];
        let profile = self.context;
        self.rows::<RawManifest>(Statement::AddManifest0, values)
            .await?
            .into_iter()
            .next()
            .ok_or(AttemptError::Invariant)?
            .decode(profile)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_manifest(
        &mut self,
        attempt: AttemptId,
        id: ManifestId,
    ) -> Result<Option<Manifest>, AttemptError> {
        let context = self.context;
        self.rows::<RawManifest>(
            Statement::GetManifest0,
            vec![Value::uuid(attempt.0), Value::uuid(id.0)],
        )
        .await?
        .into_iter()
        .next()
        .map(|value| value.decode(context))
        .transpose()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn add_evidence(
        &mut self,
        input: AddEvidence<'_>,
    ) -> Result<EvidenceId, AttemptError> {
        let (user, service) = actor(input.principal);
        let values = vec![
            Value::uuid(input.project_id.0),
            Value::uuid(input.attempt_id.0),
            Value::text(Some(input.stage)),
            Value::text(Some(input.status)),
            self.json(input.content)?,
            Value::text(Some(input.sha256)),
            Value::Uuid(input.manifest_id.map(|id| id.0)),
            Value::Uuid(user.map(|id| id.0)),
            Value::Uuid(service.map(|id| id.0)),
            Value::text(Some(channel(input.principal.via().channel))),
            Value::text(input.principal.via().client.as_deref()),
            Value::uuid(input.attempt_id.0),
            Value::text(Some(input.stage)),
            Value::text(Some(input.body)),
        ];
        self.uuid(Statement::AddEvidence0, values)
            .await?
            .map(EvidenceId)
            .ok_or(AttemptError::Invariant)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_evidence_by_id(
        &mut self,
        attempt: AttemptId,
        id: EvidenceId,
    ) -> Result<Option<(StoredJson, String)>, AttemptError> {
        let (_, rows) = self
            .execute(
                Statement::GetEvidenceById0,
                vec![Value::uuid(attempt.0), Value::uuid(id.0)],
            )
            .await?;
        rows.first()
            .map(|row| {
                Ok((
                    self.row_json(row, 0)?,
                    row.try_get(1)
                        .map_err(|error| AttemptError::database(&error))?,
                ))
            })
            .transpose()
    }
    /// A phase output's front matter, its Markdown body and its digest.
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_output_by_id(
        &mut self,
        attempt: AttemptId,
        id: EvidenceId,
    ) -> Result<Option<(StoredJson, String, String)>, AttemptError> {
        let (_, rows) = self
            .execute(
                Statement::GetEvidenceById0,
                vec![Value::uuid(attempt.0), Value::uuid(id.0)],
            )
            .await?;
        rows.first()
            .map(|row| {
                Ok((
                    self.row_json(row, 0)?,
                    row.try_get(2)
                        .map_err(|error| AttemptError::database(&error))?,
                    row.try_get(1)
                        .map_err(|error| AttemptError::database(&error))?,
                ))
            })
            .transpose()
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn get_evidence(
        &mut self,
        attempt: AttemptId,
        stage: &str,
    ) -> Result<Option<(EvidenceId, StoredJson, String)>, AttemptError> {
        let (_, rows) = self
            .execute(
                Statement::GetEvidence0,
                vec![Value::uuid(attempt.0), Value::text(Some(stage))],
            )
            .await?;
        rows.first()
            .map(|row| {
                Ok((
                    EvidenceId(
                        row.try_get(0)
                            .map_err(|error| AttemptError::database(&error))?,
                    ),
                    self.row_json(row, 1)?,
                    row.try_get(2)
                        .map_err(|error| AttemptError::database(&error))?,
                ))
            })
            .transpose()
    }
    fn row_json(&self, row: &PgRow, index: usize) -> Result<StoredJson, AttemptError> {
        let value: Option<JsonbText> = row
            .try_get(index)
            .map_err(|error| AttemptError::database(&error))?;
        wire::stored(value.as_ref().map(|value| value.0.as_str()), self.context)
    }
    fn logs(&self, logs: Option<&Document>) -> Result<Value, AttemptError> {
        if let Some(logs) = logs
            && node_truthy(logs, logs.root())
        {
            return self.json(logs);
        }
        self.json(
            &json::decode(b"[]", self.context.decode_nesting_budget)
                .map_err(|_| AttemptError::CorruptData)?,
        )
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn record_failure(
        &mut self,
        input: RecordFailure<'_>,
    ) -> Result<ReviewCaseId, AttemptError> {
        let attempt = input.attempt;
        let state = attempt.state.as_str();
        if input.from_state.is_some_and(|from| from != state) {
            return Err(AttemptError::StaleLease);
        }
        if !is_open(attempt.state) {
            return Err(AttemptError::Conflict);
        }
        if matches!(
            attempt.state,
            State::Claimed | State::Running | State::WaitingOnHuman
        ) {
            self.end_lease(attempt.id, "failed").await?;
        } else {
            self.move_attempt(attempt.id, state, "failed").await?;
        }
        let values = vec![
            Value::uuid(attempt.id.0),
            Value::text(Some(input.stage)),
            Value::text(Some(input.code)),
            Value::text(Some(input.reason)),
            self.json(input.details)?,
            self.logs(input.log_refs)?,
        ];
        let failure = self
            .uuid(Statement::RecordFailure0, values)
            .await?
            .ok_or(AttemptError::Invariant)?;
        self.execute(
            Statement::RecordFailure1,
            vec![Value::uuid(attempt.unit_id.0)],
        )
        .await?;
        self.uuid(
            Statement::RecordFailure2,
            vec![
                Value::uuid(input.project_id.0),
                Value::uuid(attempt.unit_id.0),
                Value::uuid(attempt.id.0),
                Value::uuid(failure),
                Value::uuid(attempt.id.0),
            ],
        )
        .await?
        .map(ReviewCaseId)
        .ok_or(AttemptError::Invariant)
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn requeue_failed(&mut self, input: RequeueFailed<'_>) -> Result<(), AttemptError> {
        self.end_lease(input.attempt.id, "failed").await?;
        let values = vec![
            Value::uuid(input.attempt.id.0),
            Value::text(Some(input.code)),
            Value::text(Some(input.reason)),
            self.json(input.details)?,
            self.logs(input.log_refs)?,
        ];
        self.execute(Statement::RequeueFailed0, values).await?;
        self.execute(
            Statement::RequeueFailed1,
            vec![Value::uuid(input.attempt.unit_id.0)],
        )
        .await?;
        Ok(())
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn automatic_requeues(&mut self, unit: UnitId) -> Result<i64, AttemptError> {
        self.count(Statement::AutomaticRequeues0, vec![Value::uuid(unit.0)])
            .await
    }
    /// Insertion-order groups, matching the source dict's first row appearance.
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn list_failures(
        &mut self,
        attempt_ids: &[AttemptId],
    ) -> Result<Vec<(AttemptId, Vec<Failure>)>, AttemptError> {
        if attempt_ids.is_empty() {
            return Ok(Vec::new());
        }
        let context = self.context;
        let (_, rows) = self
            .execute(
                Statement::ListFailures0,
                vec![Value::UuidList(Some(
                    attempt_ids.iter().map(|id| id.0).collect(),
                ))],
            )
            .await?;
        crate::raw::group_failures(rows.into_iter().map(|row| {
            let id = row
                .try_get("attempt_id!: AttemptId")
                .map_err(|error| AttemptError::database(&error))?;
            let failure = RawFailure::from_row(&row)
                .map_err(|error| AttemptError::database(&error))?
                .decode(context)?;
            Ok((id, failure))
        }))
    }
    /// # Errors
    /// Returns sanitized database, stored-data, source conversion, or lease errors.
    pub async fn last_failure_code(
        &mut self,
        attempt: AttemptId,
    ) -> Result<Option<String>, AttemptError> {
        self.execute(Statement::LastFailureCode0, vec![Value::uuid(attempt.0)])
            .await?
            .1
            .first()
            .map(|row| {
                row.try_get(0)
                    .map_err(|error| AttemptError::database(&error))
            })
            .transpose()
    }
}
fn channel(channel: Channel) -> &'static str {
    match channel {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    }
}
fn is_open(state: State) -> bool {
    matches!(
        state,
        State::Claimed | State::Running | State::WaitingOnHuman | State::Verifying
    )
}
fn truthy(value: &StoredJson) -> bool {
    match value {
        StoredJson::SqlNull => false,
        StoredJson::Value(value) => node_truthy(value, value.root()),
    }
}
fn node_truthy(value: &Document, id: json::NodeId) -> bool {
    match value.node(id) {
        Some(Node::Null) | None => false,
        Some(Node::Bool(value)) => *value,
        Some(Node::Integer(value)) => *value != BigInt::from(0),
        Some(Node::Float(value)) => *value != 0.0,
        Some(Node::String(value)) => !value.codepoints().is_empty(),
        Some(Node::Array(value)) => !value.is_empty(),
        Some(Node::Object(value)) => !value.is_empty(),
    }
}
pub struct Control {
    pub id: String,
    pub revision: String,
}
pub struct CreateAttempt<'a> {
    pub unit_id: UnitId,
    pub science_revision: &'a BigInt,
    pub producer: &'a Document,
    pub token_hash: &'a [u8],
    pub ttl_seconds: &'a BigInt,
    pub principal: &'a Principal,
    pub workflow: Option<&'a Document>,
    pub deadline_seconds: Option<&'a BigInt>,
}
pub struct CreateUpload<'a> {
    pub attempt_id: AttemptId,
    pub lease_generation: &'a BigInt,
    pub token_hash: &'a [u8],
    pub role: &'a str,
    pub backend: &'a str,
    pub bucket: &'a str,
    pub key: &'a str,
    pub declared_size: &'a BigInt,
    pub declared_sha256: &'a str,
    pub media_type: &'a str,
    pub ttl_minutes: &'a BigInt,
    pub max_stream_seconds: &'a BigInt,
    pub job_id: Option<JobId>,
    pub interface: Option<&'a str>,
    pub transfer: &'a str,
    pub multipart_upload_id: Option<&'a str>,
    pub part_size: Option<&'a BigInt>,
    pub slot: Option<&'a str>,
}
pub struct AddArtifact<'a> {
    pub project_id: ProjectId,
    pub upload: &'a Upload,
    pub size_bytes: &'a BigInt,
    pub sha256: &'a str,
    pub generation: Option<&'a str>,
    pub content_validated: Option<bool>,
}
pub struct AddEvidence<'a> {
    pub project_id: ProjectId,
    pub attempt_id: AttemptId,
    pub stage: &'a str,
    pub status: &'a str,
    pub content: &'a Document,
    /// The output document's Markdown body; empty for an evidence record.
    pub body: &'a str,
    pub sha256: &'a str,
    pub manifest_id: Option<ManifestId>,
    pub principal: &'a Principal,
}
pub struct RecordFailure<'a> {
    pub project_id: ProjectId,
    pub attempt: &'a Attempt,
    pub stage: &'a str,
    pub code: &'a str,
    pub reason: &'a str,
    pub details: &'a Document,
    pub log_refs: Option<&'a Document>,
    pub from_state: Option<&'a str>,
}
pub struct RequeueFailed<'a> {
    pub attempt: &'a Attempt,
    pub code: &'a str,
    pub reason: &'a str,
    pub details: &'a Document,
    pub log_refs: Option<&'a Document>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Control([redacted])")
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
