//! Lossless stored values; no authorization or lifecycle policy is inferred.
use crate::repo::AttemptError;
use cannery_core::{
    ids::{AttemptId, HypothesisId, JobId, ProjectId, ServiceAccountId, TrackId, UserId},
    json::{Document, Node},
    principal::Principal,
    timestamps::Timestamp,
};
use std::sync::Arc;
use uuid::Uuid;
macro_rules! ids {($($name:ident),+)=>{$(#[derive(Clone,Copy,Debug,Eq,PartialEq,Ord,PartialOrd,sqlx::Type)] #[sqlx(transparent)] pub struct $name(pub Uuid);)+};}
ids!(UploadId, ArtifactId, ManifestId, EvidenceId, FailureId);
macro_rules! domain {($name:ident {$($variant:ident=>$value:literal),+})=>{
#[derive(Clone,Copy,Debug,Eq,PartialEq)] pub enum $name {$($variant),+}
impl $name {#[must_use] pub const fn as_str(self)->&'static str {match self {$(Self::$variant=>$value),+}} pub(crate) fn parse(value:&str)->Result<Self,AttemptError> {match value {$($value=>Ok(Self::$variant)),+,_=>Err(AttemptError::CorruptData)}}}
};}
domain!(State {Claimed=>"claimed",Running=>"running",Submitted=>"submitted",Testing=>"testing",Evaluating=>"evaluating",AwaitingHumanReview=>"awaiting_human_review",Promoted=>"promoted",Rejected=>"rejected",Inconclusive=>"inconclusive",Failed=>"failed",Cancelled=>"cancelled",Unreviewed=>"unreviewed"});
domain!(UploadState {Pending=>"pending",Receiving=>"receiving",Verified=>"verified",Failed=>"failed",Expired=>"expired"});
domain!(Transfer {Stream=>"stream",Single=>"single",Multipart=>"multipart"});
domain!(Origin {Live=>"live",Imported=>"imported"});
domain!(Stage {Agent=>"agent",Tester=>"tester",Evaluator=>"evaluator"});
domain!(TrackState {Active=>"active",Paused=>"paused",Archived=>"archived"});
domain!(Mode {Agent=>"agent",Workflow=>"workflow"});
/// SQL NULL and a stored JSON null remain distinguishable. Consumers may ask
/// separately for the source Python None interpretation.
#[derive(Clone)]
pub enum StoredJson {
    SqlNull,
    Value(Arc<Document>),
}
impl StoredJson {
    #[must_use]
    pub fn python_none(&self) -> bool {
        match self {
            Self::SqlNull => true,
            Self::Value(value) => matches!(value.node(value.root()), Some(Node::Null)),
        }
    }
}
impl std::fmt::Debug for StoredJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoredJson([redacted])")
    }
}
/// Calibrated source JSON entry profile, selected explicitly by the caller.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub encode_nesting_budget: usize,
    pub decode_nesting_budget: usize,
}
pub struct Attempt {
    pub id: AttemptId,
    pub project_id: ProjectId,
    pub hypothesis_id: HypothesisId,
    pub hypothesis_number: i32,
    pub sequence: i32,
    pub state: State,
    pub hypothesis_revision: i32,
    pub science_revision: i32,
    pub track_id: TrackId,
    pub track_slug: String,
    pub producer: StoredJson,
    pub claimed_by_user: Option<UserId>,
    pub claimed_by_service: Option<ServiceAccountId>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub predecessor_id: Option<AttemptId>,
    pub lease_generation: i32,
    pub lease_token_hash: Option<Vec<u8>>,
    pub lease_expires_at: Option<Timestamp>,
    pub claimed_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub submitted_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub origin: Origin,
    pub source_ref: Option<String>,
    pub imported: StoredJson,
    pub workflow: StoredJson,
    pub deadline: Option<Timestamp>,
}
impl Attempt {
    #[must_use]
    pub fn mode(&self) -> Mode {
        if self.workflow.python_none() {
            Mode::Agent
        } else {
            Mode::Workflow
        }
    }
}
#[must_use]
pub fn is_claimant(principal: &Principal, attempt: &Attempt) -> bool {
    let (user, service) = actor(principal);
    (user, service) == (attempt.claimed_by_user, attempt.claimed_by_service)
}
pub(crate) const fn actor(principal: &Principal) -> (Option<UserId>, Option<ServiceAccountId>) {
    match principal {
        Principal::User(user) => (Some(user.user_id), None),
        Principal::Service(service) => (None, Some(service.service_account_id)),
    }
}
pub struct Upload {
    pub id: UploadId,
    pub attempt_id: AttemptId,
    pub lease_generation: i32,
    pub token_hash: Vec<u8>,
    pub role: String,
    pub backend: String,
    pub bucket: String,
    pub key: String,
    pub declared_size: i64,
    pub declared_sha256: String,
    pub media_type: String,
    pub state: UploadState,
    pub expires_at: Timestamp,
    pub job_id: Option<JobId>,
    pub interface: Option<String>,
    pub transfer: Transfer,
    pub multipart_upload_id: Option<String>,
    pub part_size: Option<i64>,
    pub urls_expire_at: Option<Timestamp>,
}
pub struct Artifact {
    pub id: ArtifactId,
    pub attempt_id: AttemptId,
    pub role: String,
    pub backend: String,
    pub bucket: String,
    pub key: String,
    pub generation: Option<String>,
    pub size_bytes: i64,
    pub sha256: String,
    pub media_type: String,
    pub verified_at: Timestamp,
    pub job_id: Option<JobId>,
    pub interface: Option<String>,
    pub content_validated: Option<bool>,
    pub origin: Origin,
    pub source_ref: Option<String>,
    pub uri: Option<String>,
}
pub struct Manifest {
    pub id: ManifestId,
    pub attempt_id: AttemptId,
    pub stage: Stage,
    pub content: StoredJson,
    pub sha256: String,
    pub created_at: Timestamp,
}
pub struct Failure {
    pub id: FailureId,
    pub stage: Stage,
    pub code: String,
    pub reason: String,
    pub details: StoredJson,
    pub created_at: Timestamp,
    pub requeued: bool,
    pub log_refs: StoredJson,
}
pub struct RefusedUpload {
    pub id: UploadId,
    pub role: String,
    pub key: String,
    pub interface: Option<String>,
    pub refusal: StoredJson,
}
pub struct TrackPin {
    pub slug: String,
    pub state: TrackState,
    pub producer: StoredJson,
    pub mode: Mode,
    pub workflow: StoredJson,
}
macro_rules! redacted {($($name:ident),+)=>{$(impl std::fmt::Debug for $name {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(concat!(stringify!($name),"([redacted])"))}})+};}
redacted!(
    Attempt,
    Upload,
    Artifact,
    Manifest,
    Failure,
    RefusedUpload,
    TrackPin
);
