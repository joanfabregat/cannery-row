use crate::{
    model::{
        Artifact, ArtifactId, Attempt, Failure, FailureId, JsonContext, Manifest, ManifestId, Mode,
        Origin, RefusedUpload, Stage, State, TrackPin, TrackState, Transfer, Upload, UploadId,
        UploadState,
    },
    repo::AttemptError,
    wire::{self, JsonbText},
};
use cannery_core::{
    ids::{AttemptId, JobId, ProjectId, ServiceAccountId, TrackId, UnitId, UserId},
    timestamps::Timestamp,
};
#[derive(sqlx::FromRow)]
pub(crate) struct RawAttempt {
    #[sqlx(rename = "id!: AttemptId")]
    pub id: AttemptId,
    #[sqlx(rename = "project_id!: ProjectId")]
    pub project_id: ProjectId,
    #[sqlx(rename = "unit_id!: UnitId")]
    pub unit_id: UnitId,
    #[sqlx(rename = "unit_number!")]
    pub unit_number: i32,
    #[sqlx(rename = "sequence!")]
    pub sequence: i32,
    #[sqlx(rename = "state!")]
    pub state: String,
    #[sqlx(rename = "unit_revision!")]
    pub unit_revision: i32,
    #[sqlx(rename = "science_revision!")]
    pub science_revision: i32,
    #[sqlx(rename = "track_id!: TrackId")]
    pub track_id: TrackId,
    #[sqlx(rename = "track_slug!")]
    pub track_slug: String,
    #[sqlx(rename = "producer?: JsonbText")]
    pub producer: Option<JsonbText>,
    #[sqlx(rename = "claimed_by_user?: UserId")]
    pub claimed_by_user: Option<UserId>,
    #[sqlx(rename = "claimed_by_service?: ServiceAccountId")]
    pub claimed_by_service: Option<ServiceAccountId>,
    #[sqlx(rename = "via_channel!")]
    pub via_channel: String,
    #[sqlx(rename = "via_client?")]
    pub via_client: Option<String>,
    #[sqlx(rename = "predecessor_id?: AttemptId")]
    pub predecessor_id: Option<AttemptId>,
    #[sqlx(rename = "lease_generation!")]
    pub lease_generation: i32,
    #[sqlx(rename = "lease_token_hash?")]
    pub lease_token_hash: Option<Vec<u8>>,
    #[sqlx(rename = "lease_expires_at?: Timestamp")]
    pub lease_expires_at: Option<Timestamp>,
    #[sqlx(rename = "claimed_at!: Timestamp")]
    pub claimed_at: Timestamp,
    #[sqlx(rename = "started_at?: Timestamp")]
    pub started_at: Option<Timestamp>,
    #[sqlx(rename = "submitted_at?: Timestamp")]
    pub submitted_at: Option<Timestamp>,
    #[sqlx(rename = "finished_at?: Timestamp")]
    pub finished_at: Option<Timestamp>,
    #[sqlx(rename = "origin!")]
    pub origin: String,
    #[sqlx(rename = "source_ref?")]
    pub source_ref: Option<String>,
    #[sqlx(rename = "imported?: JsonbText")]
    pub imported: Option<JsonbText>,
    #[sqlx(rename = "workflow?: JsonbText")]
    pub workflow: Option<JsonbText>,
    #[sqlx(rename = "deadline?: Timestamp")]
    pub deadline: Option<Timestamp>,
}
impl RawAttempt {
    pub(crate) fn decode(self, c: JsonContext) -> Result<Attempt, AttemptError> {
        Ok(Attempt {
            id: self.id,
            project_id: self.project_id,
            unit_id: self.unit_id,
            unit_number: self.unit_number,
            sequence: self.sequence,
            state: State::parse(&self.state)?,
            unit_revision: self.unit_revision,
            science_revision: self.science_revision,
            track_id: self.track_id,
            track_slug: self.track_slug,
            producer: wire::stored(self.producer.as_ref().map(|value| value.0.as_str()), c)?,
            claimed_by_user: self.claimed_by_user,
            claimed_by_service: self.claimed_by_service,
            via_channel: self.via_channel,
            via_client: self.via_client,
            predecessor_id: self.predecessor_id,
            lease_generation: self.lease_generation,
            lease_token_hash: self.lease_token_hash,
            lease_expires_at: self.lease_expires_at,
            claimed_at: self.claimed_at,
            started_at: self.started_at,
            submitted_at: self.submitted_at,
            finished_at: self.finished_at,
            origin: Origin::parse(&self.origin)?,
            source_ref: self.source_ref,
            imported: wire::stored(self.imported.as_ref().map(|value| value.0.as_str()), c)?,
            workflow: wire::stored(self.workflow.as_ref().map(|value| value.0.as_str()), c)?,
            deadline: self.deadline,
        })
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawUpload {
    #[sqlx(rename = "id!: UploadId")]
    pub id: UploadId,
    #[sqlx(rename = "attempt_id!: AttemptId")]
    pub attempt_id: AttemptId,
    #[sqlx(rename = "lease_generation!")]
    pub lease_generation: i32,
    #[sqlx(rename = "token_hash!")]
    pub token_hash: Vec<u8>,
    #[sqlx(rename = "role!")]
    pub role: String,
    #[sqlx(rename = "backend!")]
    pub backend: String,
    #[sqlx(rename = "bucket!")]
    pub bucket: String,
    #[sqlx(rename = "key!")]
    pub key: String,
    #[sqlx(rename = "declared_size!")]
    pub declared_size: i64,
    #[sqlx(rename = "declared_sha256!")]
    pub declared_sha256: String,
    #[sqlx(rename = "media_type!")]
    pub media_type: String,
    #[sqlx(rename = "state!")]
    pub state: String,
    #[sqlx(rename = "expires_at!: Timestamp")]
    pub expires_at: Timestamp,
    #[sqlx(rename = "job_id?: JobId")]
    pub job_id: Option<JobId>,
    #[sqlx(rename = "interface?")]
    pub interface: Option<String>,
    #[sqlx(rename = "transfer!")]
    pub transfer: String,
    #[sqlx(rename = "multipart_upload_id?")]
    pub multipart_upload_id: Option<String>,
    #[sqlx(rename = "part_size?")]
    pub part_size: Option<i64>,
    #[sqlx(rename = "urls_expire_at?: Timestamp")]
    pub urls_expire_at: Option<Timestamp>,
}
impl RawUpload {
    pub(crate) fn decode(self, _c: JsonContext) -> Result<Upload, AttemptError> {
        Ok(Upload {
            id: self.id,
            attempt_id: self.attempt_id,
            lease_generation: self.lease_generation,
            token_hash: self.token_hash,
            role: self.role,
            backend: self.backend,
            bucket: self.bucket,
            key: self.key,
            declared_size: self.declared_size,
            declared_sha256: self.declared_sha256,
            media_type: self.media_type,
            state: UploadState::parse(&self.state)?,
            expires_at: self.expires_at,
            job_id: self.job_id,
            interface: self.interface,
            transfer: Transfer::parse(&self.transfer)?,
            multipart_upload_id: self.multipart_upload_id,
            part_size: self.part_size,
            urls_expire_at: self.urls_expire_at,
        })
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawArtifact {
    #[sqlx(rename = "id!: ArtifactId")]
    pub id: ArtifactId,
    #[sqlx(rename = "attempt_id!: AttemptId")]
    pub attempt_id: AttemptId,
    #[sqlx(rename = "role!")]
    pub role: String,
    #[sqlx(rename = "backend!")]
    pub backend: String,
    #[sqlx(rename = "bucket!")]
    pub bucket: String,
    #[sqlx(rename = "key!")]
    pub key: String,
    #[sqlx(rename = "generation?")]
    pub generation: Option<String>,
    #[sqlx(rename = "size_bytes!")]
    pub size_bytes: i64,
    #[sqlx(rename = "sha256!")]
    pub sha256: String,
    #[sqlx(rename = "media_type!")]
    pub media_type: String,
    #[sqlx(rename = "verified_at!: Timestamp")]
    pub verified_at: Timestamp,
    #[sqlx(rename = "job_id?: JobId")]
    pub job_id: Option<JobId>,
    #[sqlx(rename = "interface?")]
    pub interface: Option<String>,
    #[sqlx(rename = "content_validated?")]
    pub content_validated: Option<bool>,
    #[sqlx(rename = "origin!")]
    pub origin: String,
    #[sqlx(rename = "source_ref?")]
    pub source_ref: Option<String>,
    #[sqlx(rename = "uri?")]
    pub uri: Option<String>,
}
impl RawArtifact {
    pub(crate) fn decode(self, _c: JsonContext) -> Result<Artifact, AttemptError> {
        Ok(Artifact {
            id: self.id,
            attempt_id: self.attempt_id,
            role: self.role,
            backend: self.backend,
            bucket: self.bucket,
            key: self.key,
            generation: self.generation,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
            media_type: self.media_type,
            verified_at: self.verified_at,
            job_id: self.job_id,
            interface: self.interface,
            content_validated: self.content_validated,
            origin: Origin::parse(&self.origin)?,
            source_ref: self.source_ref,
            uri: self.uri,
        })
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawManifest {
    #[sqlx(rename = "id!: ManifestId")]
    pub id: ManifestId,
    #[sqlx(rename = "attempt_id!: AttemptId")]
    pub attempt_id: AttemptId,
    #[sqlx(rename = "stage!")]
    pub stage: String,
    #[sqlx(rename = "content!: JsonbText")]
    pub content: JsonbText,
    #[sqlx(rename = "sha256!")]
    pub sha256: String,
    #[sqlx(rename = "created_at!: Timestamp")]
    pub created_at: Timestamp,
}
impl RawManifest {
    pub(crate) fn decode(self, c: JsonContext) -> Result<Manifest, AttemptError> {
        Ok(Manifest {
            id: self.id,
            attempt_id: self.attempt_id,
            stage: Stage::parse(&self.stage)?,
            content: wire::stored(Some(self.content.0.as_str()), c)?,
            sha256: self.sha256,
            created_at: self.created_at,
        })
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawFailure {
    #[sqlx(rename = "id!: FailureId")]
    pub id: FailureId,
    #[sqlx(rename = "stage!")]
    pub stage: String,
    #[sqlx(rename = "code!")]
    pub code: String,
    #[sqlx(rename = "reason!")]
    pub reason: String,
    #[sqlx(rename = "details!: JsonbText")]
    pub details: JsonbText,
    #[sqlx(rename = "created_at!: Timestamp")]
    pub created_at: Timestamp,
    #[sqlx(rename = "requeued!")]
    pub requeued: bool,
    #[sqlx(rename = "log_refs!: JsonbText")]
    pub log_refs: JsonbText,
}
impl RawFailure {
    pub(crate) fn decode(self, c: JsonContext) -> Result<Failure, AttemptError> {
        Ok(Failure {
            id: self.id,
            stage: Stage::parse(&self.stage)?,
            code: self.code,
            reason: self.reason,
            details: wire::stored(Some(self.details.0.as_str()), c)?,
            created_at: self.created_at,
            requeued: self.requeued,
            log_refs: wire::stored(Some(self.log_refs.0.as_str()), c)?,
        })
    }
}
pub(crate) fn group_failures<E>(
    rows: impl IntoIterator<Item = Result<(AttemptId, Failure), E>>,
) -> Result<Vec<(AttemptId, Vec<Failure>)>, E> {
    let mut groups: Vec<(AttemptId, Vec<Failure>)> = Vec::new();
    let mut positions = std::collections::BTreeMap::new();
    for row in rows {
        let (id, failure) = row?;
        if let Some(&index) = positions.get(&id) {
            let index: usize = index;
            groups[index].1.push(failure);
        } else {
            positions.insert(id, groups.len());
            groups.push((id, vec![failure]));
        }
    }
    Ok(groups)
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawRefusedUpload {
    #[sqlx(rename = "id!: UploadId")]
    pub id: UploadId,
    #[sqlx(rename = "role!")]
    pub role: String,
    #[sqlx(rename = "key!")]
    pub key: String,
    #[sqlx(rename = "interface?")]
    pub interface: Option<String>,
    #[sqlx(rename = "refusal!: JsonbText")]
    pub refusal: JsonbText,
}
impl RawRefusedUpload {
    pub(crate) fn decode(self, c: JsonContext) -> Result<RefusedUpload, AttemptError> {
        Ok(RefusedUpload {
            id: self.id,
            role: self.role,
            key: self.key,
            interface: self.interface,
            refusal: wire::stored(Some(self.refusal.0.as_str()), c)?,
        })
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct RawTrackPin {
    #[sqlx(rename = "slug!")]
    pub slug: String,
    #[sqlx(rename = "state!")]
    pub state: String,
    #[sqlx(rename = "producer?: JsonbText")]
    pub producer: Option<JsonbText>,
    #[sqlx(rename = "mode!")]
    pub mode: String,
    #[sqlx(rename = "workflow?: JsonbText")]
    pub workflow: Option<JsonbText>,
}
impl RawTrackPin {
    pub(crate) fn decode(self, c: JsonContext) -> Result<TrackPin, AttemptError> {
        Ok(TrackPin {
            slug: self.slug,
            state: TrackState::parse(&self.state)?,
            producer: wire::stored(self.producer.as_ref().map(|value| value.0.as_str()), c)?,
            mode: Mode::parse(&self.mode)?,
            workflow: wire::stored(self.workflow.as_ref().map(|value| value.0.as_str()), c)?,
        })
    }
}
