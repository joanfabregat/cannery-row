//! Typed REST request and response contracts. `OpenAPI` derives from these models.
#![allow(non_camel_case_types)]
use std::collections::BTreeMap;

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub role: String,
    pub storage: StorageRef,
    pub size_bytes: i64,
    pub sha256: String,
    pub media_type: String,
    #[schema(format = "date-time")]
    pub verified_at: String,
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub content_validated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptDetail {
    #[schema(format = "uuid")]
    pub id: String,
    pub r#ref: String,
    pub number: i64,
    pub sequence: i64,
    pub state: String,
    pub track: String,
    pub hypothesis_revision: i64,
    pub science_revision: i64,
    #[schema(required = true)]
    /// Legacy stored references remain readable; new requests use `RequestCommonProducerRef`.
    pub producer: Option<BTreeMap<String, serde_json::Value>>,
    pub mode: TrackMode,
    #[schema(required = true)]
    /// Legacy stored workflow metadata remains readable; new requests use `TrackCreateRequestWorkflow`.
    pub workflow: Option<BTreeMap<String, serde_json::Value>>,
    pub claimed_by: Claimant,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "uuid", required = true)]
    pub predecessor_id: Option<String>,
    pub lease_generation: i64,
    #[schema(format = "date-time", required = true)]
    pub lease_expires_at: Option<String>,
    #[schema(format = "date-time")]
    pub claimed_at: String,
    #[schema(format = "date-time", required = true)]
    pub started_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub submitted_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub finished_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    #[schema(required = true)]
    pub imported: Option<BTreeMap<String, serde_json::Value>>,
    pub artifacts: Vec<ArtifactOut>,
    pub failures: Vec<cannery_row__attempts__routes__FailureOut>,
    #[schema(required = true)]
    pub claimed_sheet: Option<ReadEvidenceEnvelope>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub r#ref: String,
    pub number: i64,
    pub sequence: i64,
    pub state: String,
    pub track: String,
    pub hypothesis_revision: i64,
    pub science_revision: i64,
    #[schema(required = true)]
    /// Legacy stored references remain readable; new requests use `RequestCommonProducerRef`.
    pub producer: Option<BTreeMap<String, serde_json::Value>>,
    pub mode: TrackMode,
    #[schema(required = true)]
    /// Legacy stored workflow metadata remains readable; new requests use `TrackCreateRequestWorkflow`.
    pub workflow: Option<BTreeMap<String, serde_json::Value>>,
    pub claimed_by: Claimant,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "uuid", required = true)]
    pub predecessor_id: Option<String>,
    pub lease_generation: i64,
    #[schema(format = "date-time", required = true)]
    pub lease_expires_at: Option<String>,
    #[schema(format = "date-time")]
    pub claimed_at: String,
    #[schema(format = "date-time", required = true)]
    pub started_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub submitted_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub finished_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    #[schema(required = true)]
    pub imported: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum AttemptState {
    #[serde(rename = "claimed")]
    Claimed,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "submitted")]
    Submitted,
    #[serde(rename = "validating")]
    Validating,
    #[serde(rename = "testing")]
    Testing,
    #[serde(rename = "evaluating")]
    Evaluating,
    #[serde(rename = "awaiting_human_review")]
    AwaitingHumanReview,
    #[serde(rename = "promoted")]
    Promoted,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "inconclusive")]
    Inconclusive,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "unreviewed")]
    Unreviewed,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionFailure {
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub title: String,
    pub hypothesis_state: String,
    pub track: String,
    pub attempt_ref: String,
    pub stage: String,
    pub code: String,
    pub reason: String,
    #[schema(format = "date-time")]
    pub created_at: String,
    pub origin: Origin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionOut {
    pub pending_counts: BTreeMap<String, i64>,
    pub pending_reviews: Vec<AttentionReview>,
    pub running_count: i64,
    pub running: Vec<AttentionRunning>,
    pub recent_outcomes: Vec<AttentionOutcome>,
    pub recent_failures: Vec<AttentionFailure>,
    pub stalled_evaluation_count: i64,
    pub stalled_evaluations: Vec<AttentionStalledEvaluation>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionOutcome {
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub title: String,
    pub hypothesis_state: String,
    pub track: String,
    #[schema(required = true)]
    pub attempt_ref: Option<String>,
    pub action: String,
    pub reason: String,
    #[schema(format = "date-time")]
    pub decided_at: String,
    pub origin: Origin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionReview {
    #[schema(format = "uuid")]
    pub case_id: String,
    pub kind: String,
    pub subject_revision: i64,
    #[schema(format = "date-time")]
    pub opened_at: String,
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub title: String,
    pub track: String,
    #[schema(required = true)]
    pub attempt_ref: Option<String>,
    #[schema(required = true)]
    pub verdict: Option<String>,
    #[schema(required = true)]
    pub failure_stage: Option<String>,
    #[schema(required = true)]
    pub failure_code: Option<String>,
    #[schema(required = true)]
    pub failure_reason: Option<String>,
    pub origin: Origin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionRunning {
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub title: String,
    pub track: String,
    pub attempt_ref: String,
    pub state: String,
    #[schema(format = "date-time")]
    pub claimed_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AttentionStalledEvaluation {
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub title: String,
    pub track: String,
    pub attempt_ref: String,
    pub evaluator: String,
    pub revision: String,
    #[schema(format = "date-time")]
    pub waiting_since: String,
    pub message: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub kind: String,
    #[schema(format = "uuid")]
    pub id: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub id: String,
    pub revision: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum CaseKind {
    #[serde(rename = "draft")]
    Draft,
    #[serde(rename = "result")]
    Result,
    #[serde(rename = "failure")]
    Failure,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum CaseState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "resolved")]
    Resolved,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogOut {
    pub science_revision: i64,
    pub metrics: Vec<RequestScienceRevisionMetric>,
    pub baselines: Vec<Baseline>,
    pub group_by: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimOut {
    pub attempt: AttemptOut,
    pub lease_token: String,
    pub lease_generation: i64,
    #[schema(format = "date-time")]
    pub lease_expires_at: String,
    pub heartbeat_seconds: i64,
    #[serde(default)]
    pub workflow: Option<ClaimedWorkflow>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    #[serde(default)]
    #[schema(minimum = 1.0, maximum = 2_147_483_647.0)]
    pub hypothesis: Option<i64>,
    #[serde(default)]
    pub track: Option<String>,
    #[serde(default)]
    pub mode: Option<TrackMode>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Claimant {
    pub kind: String,
    #[schema(format = "uuid")]
    pub id: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentCreate {
    #[schema(min_length = 1, max_length = 65536, pattern = "\\S")]
    pub body_markdown: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentEdit {
    pub expected_revision: i64,
    #[schema(min_length = 1, max_length = 65536, pattern = "\\S")]
    pub body_markdown: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    #[schema(required = true)]
    pub attempt_ref: Option<String>,
    #[schema(format = "uuid")]
    pub author_user_id: String,
    pub body_markdown: String,
    pub revision: i64,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time", required = true)]
    pub edited_at: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentRevisionOut {
    pub revision: i64,
    pub body_markdown: String,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "date-time")]
    pub created_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonOut {
    pub id: i64,
    pub attempt_ref: String,
    pub hypothesis: i64,
    pub hypothesis_title: String,
    pub hypothesis_state: String,
    pub attempt_state: String,
    pub track: String,
    pub science_revision: i64,
    pub metric: String,
    pub split: String,
    pub dimensions: BTreeMap<String, String>,
    pub value: f64,
    pub source: String,
    pub reference: Reference,
    pub verdict: Verdict,
    pub policy_revision: String,
    #[schema(format = "uuid")]
    pub evidence_id: String,
    #[schema(format = "date-time")]
    pub recorded_at: String,
    pub origin: Origin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigOut {
    pub kind: String,
    pub revision: i64,
    #[schema(required = true)]
    pub science_revision: Option<i64>,
    pub content: ConfigDocument,
    #[schema(format = "uuid")]
    pub created_by: String,
    #[schema(format = "date-time")]
    pub created_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub science_revisions: Vec<i64>,
    #[schema(required = true)]
    pub split: Option<String>,
    pub authority: String,
    pub rows: i64,
    pub measured: i64,
    pub sample_count: i64,
    pub failed_attempts: i64,
    /// Stored legacy controls may predate the fixed hypothesis control contract.
    pub controls: Vec<BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardOut {
    #[schema(required = true)]
    pub dashboard_revision: Option<i64>,
    pub science_revision: i64,
    pub derived: bool,
    pub views: Vec<RequestDashboardViewsView>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub action: String,
    pub subject_revision: i64,
    pub reason: String,
    #[schema(format = "uuid")]
    pub actor_user_id: String,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "date-time")]
    pub decided_at: String,
    #[schema(format = "uuid", required = true)]
    pub supersedes: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectUpload {
    pub transfer: String,
    #[serde(default)]
    pub request: Option<PresignedRequestOut>,
    #[serde(default)]
    pub part_size: Option<i64>,
    #[serde(default)]
    pub part_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<PresignedPartOut>>,
    pub presign_url: String,
    pub finish_url: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DisableRequest {
    #[schema(min_length = 1, max_length = 200)]
    pub reason: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftUpdate {
    pub expected_revision: i64,
    pub document: HypothesisCreateRequest,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorReport {
    pub status: String,
    #[schema(required = true)]
    pub verdict: Option<String>,
    #[schema(required = true)]
    pub reason: Option<String>,
    #[schema(required = true)]
    pub policy_revision: Option<String>,
    pub gates: Vec<RequestEvidenceEnvelopeAssessmentGatesItem>,
    pub comparisons: Vec<RequestEvidenceEnvelopeComparison>,
    pub producer: Producer,
    #[schema(format = "date-time")]
    pub published_at: String,
    #[serde(default)]
    pub source_ref: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct HTTPValidationError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Vec<ValidationError>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryEvent {
    pub seq: i64,
    #[schema(format = "date-time")]
    pub occurred_at: String,
    pub action: String,
    pub actor_kind: String,
    #[schema(format = "uuid", required = true)]
    pub actor_user_id: Option<String>,
    #[schema(format = "uuid", required = true)]
    pub actor_service_id: Option<String>,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    pub prior_state: serde_json::Value,
    pub new_state: serde_json::Value,
    #[schema(required = true)]
    pub reason: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisOut {
    pub number: i64,
    pub r#ref: String,
    pub title: String,
    pub track: String,
    pub mode: String,
    pub state: String,
    pub revision: i64,
    #[schema(required = true)]
    pub approved_revision: Option<i64>,
    pub created_by: Author,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time")]
    pub updated_at: String,
    #[schema(format = "date-time", required = true)]
    pub approved_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    #[schema(required = true)]
    pub external_id: Option<String>,
    #[schema(required = true)]
    pub imported: Option<BTreeMap<String, serde_json::Value>>,
    #[schema(format = "uuid")]
    pub id: String,
    pub project: String,
    pub document: HypothesisDocument,
    pub science_revision: i64,
    pub relations: Vec<LinkOut>,
    pub backlinks: Vec<LinkOut>,
    pub reviews: Vec<cannery_row__hypotheses__routes__ReviewCaseOut>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisPage {
    pub items: Vec<HypothesisSummary>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum HypothesisState {
    #[serde(rename = "draft")]
    Draft,
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "awaiting_human_review")]
    AwaitingHumanReview,
    #[serde(rename = "promoted")]
    Promoted,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "inconclusive")]
    Inconclusive,
    #[serde(rename = "declined")]
    Declined,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisSummary {
    pub number: i64,
    pub r#ref: String,
    pub title: String,
    pub track: String,
    pub mode: String,
    pub state: String,
    pub revision: i64,
    #[schema(required = true)]
    pub approved_revision: Option<i64>,
    pub created_by: Author,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time")]
    pub updated_at: String,
    #[schema(format = "date-time", required = true)]
    pub approved_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    #[schema(required = true)]
    pub external_id: Option<String>,
    #[schema(required = true)]
    pub imported: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobClaimOut {
    pub job: ClaimedJobDocument,
    pub attempt_ref: String,
    pub heartbeat_seconds: i64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobClaimRequest {
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    #[schema(min_length = 1, max_length = 128)]
    pub revision: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobLeaseOut {
    pub lease_generation: i64,
    #[schema(format = "date-time")]
    pub lease_expires_at: String,
    #[schema(format = "date-time")]
    pub deadline: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobOut {
    #[schema(format = "uuid")]
    pub id: String,
    #[schema(format = "uuid")]
    pub attempt_id: String,
    pub stage: String,
    pub run_number: i64,
    pub origin: String,
    #[schema(format = "uuid", required = true)]
    pub previous_run_id: Option<String>,
    pub state: String,
    pub science_revision: i64,
    pub tester: String,
    pub track: String,
    pub steps: Vec<StepRef>,
    #[schema(required = true)]
    pub parameters: Option<BTreeMap<String, serde_json::Value>>,
    pub output_prefix: String,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time", required = true)]
    pub claimed_at: Option<String>,
    #[schema(format = "uuid", required = true)]
    pub claimed_by: Option<String>,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub deadline: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub finished_at: Option<String>,
    pub lease_generation: i64,
    #[schema(format = "date-time", required = true)]
    pub lease_expires_at: Option<String>,
    #[schema(required = true)]
    pub error_step: Option<String>,
    #[schema(required = true)]
    pub error_code: Option<String>,
    #[schema(required = true)]
    pub error_reason: Option<String>,
    pub logs: Vec<LogRef>,
    #[schema(required = true)]
    pub evidence: Option<ReadEvidenceEnvelope>,
    pub outputs: Vec<ArtifactOut>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobUploadRequest {
    #[schema(pattern = "^[a-z][a-z0-9_]{0,63}$")]
    pub role: String,
    #[schema(
        max_length = 512,
        pattern = "^[A-Za-z0-9_-][A-Za-z0-9_.-]*(/[A-Za-z0-9_-][A-Za-z0-9_.-]*)*$"
    )]
    pub path: String,
    #[schema(minimum = 0.0)]
    pub size_bytes: i64,
    #[schema(pattern = "^[0-9a-f]{64}$")]
    pub sha256: String,
    #[schema(max_length = 200, pattern = "^[a-z]+/[A-Za-z0-9.+_-]+$")]
    pub media_type: String,
    #[serde(default)]
    #[schema(pattern = "^[a-z0-9][a-z0-9-]{0,62}/v[0-9]+(\\.[0-9]+)?$")]
    pub interface: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseOut {
    pub lease_generation: i64,
    #[schema(format = "date-time")]
    pub lease_expires_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkOut {
    pub kind: String,
    pub project: String,
    pub number: i64,
    pub r#ref: String,
    pub title: String,
    pub state: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LogRef {
    #[schema(min_length = 1, max_length = 1024)]
    pub key: String,
    #[schema(minimum = 0.0)]
    pub size_bytes: i64,
    #[schema(pattern = "^[0-9a-f]{64}$")]
    pub sha256: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LogoutResult {
    #[schema(required = true)]
    pub logout_url: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ManifestRef {
    pub r#ref: String,
    pub sha256: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeOut {
    pub kind: String,
    #[serde(default)]
    pub user: Option<UserOut>,
    #[serde(default)]
    pub service_account: Option<ServiceAccountOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memberships: Option<Vec<MembershipOut>>,
    #[serde(default)]
    pub csrf_token: Option<String>,
    pub scopes: Vec<String>,
    pub channel: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberOut {
    #[schema(format = "uuid")]
    pub user_id: String,
    #[schema(required = true)]
    pub email: Option<String>,
    #[schema(required = true)]
    pub display_name: Option<String>,
    pub role: String,
    #[schema(format = "date-time")]
    pub granted_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MembershipOut {
    pub project: String,
    pub title: String,
    #[schema(required = true)]
    pub role: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MembershipSet {
    pub role: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricsPage {
    pub items: Vec<PointOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
    pub context: Context,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum Origin {
    #[serde(rename = "live")]
    Live,
    #[serde(rename = "imported")]
    Imported,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum OriginFilter {
    #[serde(rename = "live")]
    Live,
    #[serde(rename = "imported")]
    Imported,
    #[serde(rename = "all")]
    All,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_AttemptOut_UUID_ {
    pub items: Vec<AttemptOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_AttemptOut_int_ {
    pub items: Vec<AttemptOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_CommentOut_UUID_ {
    pub items: Vec<CommentOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_CommentRevisionOut_int_ {
    pub items: Vec<CommentRevisionOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ComparisonOut_int_ {
    pub items: Vec<ComparisonOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ConfigOut_int_ {
    pub items: Vec<ConfigOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_HistoryEvent_int_ {
    pub items: Vec<HistoryEvent>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_JobOut_UUID_ {
    pub items: Vec<JobOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_MemberOut_UUID_ {
    pub items: Vec<MemberOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ProducerOut_str_ {
    pub items: Vec<ProducerOut>,
    #[schema(required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ProjectOut_str_ {
    pub items: Vec<ProjectOut>,
    #[schema(required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ReportSummary_UUID_ {
    pub items: Vec<ReportSummary>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_RevisionOut_int_ {
    pub items: Vec<RevisionOut>,
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_ServiceAccountOut_str_ {
    pub items: Vec<ServiceAccountOut>,
    #[schema(required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_TokenOut_UUID_ {
    pub items: Vec<TokenOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_TrackOut_str_ {
    pub items: Vec<TrackOut>,
    #[schema(required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Page_UserOut_UUID_ {
    pub items: Vec<UserOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PointOut {
    pub id: i64,
    pub attempt_ref: String,
    pub hypothesis: i64,
    pub hypothesis_title: String,
    pub hypothesis_state: String,
    pub attempt_state: String,
    pub track: String,
    pub science_revision: i64,
    pub metric: String,
    pub split: String,
    pub dimensions: BTreeMap<String, String>,
    #[schema(required = true)]
    pub value: Option<f64>,
    #[schema(required = true)]
    pub missing_reason: Option<String>,
    pub unit: String,
    pub direction: String,
    #[schema(required = true)]
    pub sample_count: Option<i64>,
    #[schema(required = true)]
    pub control_value: Option<f64>,
    #[schema(required = true)]
    /// Stored legacy controls remain readable, including empty and arbitrary objects.
    pub control: Option<BTreeMap<String, serde_json::Value>>,
    #[schema(required = true)]
    pub reference: Option<Reference>,
    #[schema(required = true)]
    pub uncertainty: Option<Uncertainty>,
    pub authority: String,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[schema(format = "date-time")]
    pub claimed_at: String,
    #[schema(format = "date-time", required = true)]
    pub finished_at: Option<String>,
    #[schema(format = "date-time")]
    pub recorded_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresignOut {
    #[serde(default)]
    pub request: Option<PresignedRequestOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<PresignedPartOut>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresignRequest {
    #[serde(default)]
    #[schema(min_items = 1, max_items = 100)]
    pub part_numbers: Option<Vec<i64>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresignedPartOut {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    #[schema(format = "date-time")]
    pub expires_at: String,
    pub part_number: i64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresignedRequestOut {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    #[schema(format = "date-time")]
    pub expires_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub kind: String,
    #[schema(format = "uuid", required = true)]
    pub id: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProducerOut {
    pub name: String,
    pub revision: i64,
    pub content: StepManifestRequest,
    #[schema(format = "uuid")]
    pub created_by: String,
    #[schema(format = "date-time")]
    pub created_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreate {
    #[schema(pattern = "^[a-z0-9][a-z0-9-]{0,62}$")]
    pub slug: String,
    #[schema(min_length = 1, max_length = 200)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub value: f64,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub r#ref: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequest {
    #[schema(min_length = 1, max_length = 4000)]
    pub reason: String,
    #[serde(default)]
    pub code: Option<RunnerFailureCode>,
    #[serde(default)]
    #[schema(pattern = "^[a-z0-9][a-z0-9-]{0,62}$")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_items = 64)]
    pub logs: Option<Vec<LogRef>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub attempt_ref: String,
    pub hypothesis: i64,
    pub hypothesis_title: String,
    pub track: String,
    pub attempt_state: String,
    pub science_revision: i64,
    pub status: String,
    pub report: ReportDocument,
    pub claimed_measurements: Vec<ReadMeasurement>,
    pub origin: Origin,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[schema(format = "date-time")]
    pub submitted_at: String,
    pub author: Producer,
    #[schema(required = true)]
    pub tester: Option<TesterReport>,
    #[schema(required = true)]
    pub evaluation: Option<EvaluatorReport>,
    pub decisions: Vec<DecisionOut>,
    pub assets: Vec<ArtifactOut>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportSummary {
    #[schema(format = "uuid")]
    pub id: String,
    pub attempt_ref: String,
    pub hypothesis: i64,
    pub hypothesis_title: String,
    pub track: String,
    pub attempt_state: String,
    pub status: String,
    pub what_was_tried: String,
    pub findings: String,
    #[schema(format = "date-time")]
    pub submitted_at: String,
    pub author: Producer,
    pub origin: Origin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum ResultDecision {
    #[serde(rename = "promote")]
    Promote,
    #[serde(rename = "reject")]
    Reject,
    #[serde(rename = "inconclusive")]
    Inconclusive,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewCasePage {
    pub items: Vec<cannery_row__reviews__routes__ReviewCaseOut>,
    #[schema(format = "uuid", required = true)]
    pub next_before: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionOut {
    pub revision: i64,
    pub science_revision: i64,
    pub author: Author,
    pub via_channel: String,
    #[schema(required = true)]
    pub via_client: Option<String>,
    #[schema(format = "date-time")]
    pub created_at: String,
    pub origin: Origin,
    pub document: HypothesisDocument,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RunnerFailureCode {
    #[serde(rename = "step_failed")]
    StepFailed,
    #[serde(rename = "deadline_exceeded")]
    DeadlineExceeded,
    #[serde(rename = "runner_error")]
    RunnerError,
    #[serde(rename = "setup_failed")]
    SetupFailed,
    #[serde(rename = "invalid_step_output")]
    InvalidStepOutput,
    #[serde(rename = "invalid_output")]
    InvalidOutput,
    #[serde(rename = "missing_output")]
    MissingOutput,
    #[serde(rename = "invalid_code")]
    InvalidCode,
    #[serde(rename = "code_not_allowed")]
    CodeNotAllowed,
    #[serde(rename = "invalid_input")]
    InvalidInput,
    #[serde(rename = "missing_input")]
    MissingInput,
    #[serde(rename = "input_verification_failed")]
    InputVerificationFailed,
    #[serde(rename = "upload_expired")]
    UploadExpired,
    #[serde(rename = "invalid_job")]
    InvalidJob,
    #[serde(rename = "held_out_labels_to_experiment")]
    HeldOutLabelsToExperiment,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum ScopeName {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub kind: String,
    #[schema(format = "uuid")]
    pub source_id: String,
    pub project: String,
    #[schema(required = true)]
    pub r#ref: Option<String>,
    #[schema(required = true)]
    pub hypothesis: Option<i64>,
    #[schema(required = true)]
    pub attempt_ref: Option<String>,
    pub title: String,
    pub snippet: String,
    #[schema(required = true)]
    pub track: Option<String>,
    #[schema(required = true)]
    pub hypothesis_state: Option<String>,
    #[schema(required = true)]
    pub attempt_state: Option<String>,
    #[schema(required = true)]
    pub verdict: Option<String>,
    #[schema(required = true)]
    pub decision: Option<String>,
    #[schema(required = true)]
    pub actor: Option<Producer>,
    #[schema(format = "date-time")]
    pub occurred_at: String,
    pub origin: Origin,
    pub score: f64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchPage {
    pub items: Vec<SearchHit>,
    #[schema(required = true)]
    pub next_before: Option<String>,
    pub total: i64,
    pub facets: BTreeMap<String, BTreeMap<String, i64>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Series {
    pub science_revision: i64,
    pub group: BTreeMap<String, serde_json::Value>,
    pub points: Vec<SeriesPoint>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SeriesPoint {
    pub x: serde_json::Value,
    #[schema(required = true)]
    pub value: Option<f64>,
    pub count: i64,
    pub attempt_refs: Vec<String>,
    #[schema(required = true)]
    pub control_value: Option<f64>,
    #[schema(required = true)]
    pub reference_label: Option<String>,
    #[schema(required = true)]
    pub uncertainty: Option<Uncertainty>,
    #[schema(required = true)]
    pub sample_count: Option<i64>,
    pub missing_reasons: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccountCreate {
    pub kind: ServiceKind,
    #[schema(pattern = "^[a-z0-9][a-z0-9-]{0,62}$")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccountOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub project: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time", required = true)]
    pub disabled_at: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum ServiceKind {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "experimenter")]
    Experimenter,
    #[serde(rename = "tester")]
    Tester,
    #[serde(rename = "evaluator")]
    Evaluator,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepRef {
    pub name: String,
    pub revision: StepRefRevisionValue,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageRef {
    pub backend: String,
    pub bucket: String,
    pub key: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterReport {
    pub status: String,
    #[schema(required = true)]
    pub observations: Option<String>,
    pub discrepancies: Vec<RequestEvidenceEnvelopeDiscrepancy>,
    pub measurements: Vec<ReadMeasurement>,
    #[schema(format = "date-time")]
    pub published_at: String,
    #[serde(default)]
    pub source_ref: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenCreate {
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    pub expires_in_days: i64,
    #[schema(min_items = 1)]
    pub scopes: Vec<ScopeName>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenCreated {
    #[schema(format = "uuid")]
    pub id: String,
    pub kind: String,
    pub name: String,
    pub display_prefix: String,
    pub scopes: Vec<String>,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time")]
    pub expires_at: String,
    #[schema(format = "date-time", required = true)]
    pub last_used_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub revoked_at: Option<String>,
    pub token: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub kind: String,
    pub name: String,
    pub display_prefix: String,
    pub scopes: Vec<String>,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time")]
    pub expires_at: String,
    #[schema(format = "date-time", required = true)]
    pub last_used_at: Option<String>,
    #[schema(format = "date-time", required = true)]
    pub revoked_at: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum TrackMode {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "workflow")]
    Workflow,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    #[schema(required = true)]
    /// Legacy stored references remain readable; new requests use `RequestCommonProducerRef`.
    pub producer: Option<BTreeMap<String, serde_json::Value>>,
    pub mode: TrackMode,
    #[schema(required = true)]
    /// Legacy stored workflow metadata remains readable; new requests use `TrackCreateRequestWorkflow`.
    pub workflow: Option<BTreeMap<String, serde_json::Value>>,
    pub state: String,
    pub revision: i64,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[schema(format = "date-time")]
    pub updated_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackUpdate {
    pub expected_revision: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub producer: Option<RequestCommonProducerRef>,
    #[serde(default)]
    pub mode: Option<TrackMode>,
    #[serde(default)]
    pub workflow: Option<TrackCreateRequestWorkflow>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Uncertainty {
    pub method: String,
    pub lower: f64,
    pub upper: f64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadGrant {
    pub upload_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub headers: BTreeMap<String, String>,
    #[schema(format = "date-time")]
    pub expires_at: String,
    pub storage: StorageRef,
    #[serde(default)]
    pub direct: Option<DirectUpload>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    #[schema(pattern = "^[a-z][a-z0-9_]{0,63}$")]
    pub role: String,
    #[schema(pattern = "^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,127}$")]
    pub name: String,
    #[schema(minimum = 0.0)]
    pub size_bytes: i64,
    #[schema(pattern = "^[0-9a-f]{64}$")]
    pub sha256: String,
    #[schema(max_length = 200, pattern = "^[a-z]+/[A-Za-z0-9.+_-]+$")]
    pub media_type: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserOut {
    #[schema(format = "uuid")]
    pub id: String,
    #[schema(required = true)]
    pub email: Option<String>,
    pub email_verified: bool,
    #[schema(required = true)]
    pub display_name: Option<String>,
    pub is_admin: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ValidationError {
    pub loc: Vec<ValidationErrorLocItemValue>,
    pub msg: String,
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum Verdict {
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "fail")]
    Fail,
    #[serde(rename = "inconclusive")]
    Inconclusive,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewOut {
    pub view: RequestDashboardViewsView,
    #[schema(required = true)]
    pub dashboard_revision: Option<i64>,
    pub metric: ViewMetric,
    #[schema(required = true)]
    pub aggregation: Option<String>,
    pub series: Vec<Series>,
    pub context: Context,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ViewMetric {
    Registered(RequestScienceRevisionMetric),
    Unregistered(UnregisteredMetric),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnregisteredMetric {}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct cannery_row__attempts__routes__FailureOut {
    pub stage: String,
    pub code: String,
    pub reason: String,
    pub details: BTreeMap<String, serde_json::Value>,
    #[schema(format = "date-time")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requeued: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_refs: Option<Vec<LogRef>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum cannery_row__config__routes__Kind {
    #[serde(rename = "science")]
    Science,
    #[serde(rename = "dashboard")]
    Dashboard,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct cannery_row__hypotheses__routes__ReviewCaseOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub kind: String,
    pub subject_revision: i64,
    pub state: String,
    #[schema(format = "date-time")]
    pub opened_at: String,
    #[schema(format = "date-time", required = true)]
    pub resolved_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    pub decisions: Vec<DecisionOut>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct cannery_row__reviews__routes__FailureOut {
    pub stage: String,
    pub code: String,
    pub reason: String,
    pub details: BTreeMap<String, serde_json::Value>,
    pub log_refs: Vec<LogRef>,
    #[schema(format = "date-time")]
    pub created_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct cannery_row__reviews__routes__ReviewCaseOut {
    #[schema(format = "uuid")]
    pub id: String,
    pub kind: String,
    pub state: String,
    pub subject_revision: i64,
    pub hypothesis: i64,
    pub hypothesis_ref: String,
    pub hypothesis_state: String,
    #[schema(required = true)]
    pub attempt_ref: Option<String>,
    #[schema(required = true)]
    pub attempt_state: Option<String>,
    #[schema(format = "date-time")]
    pub opened_at: String,
    #[schema(format = "date-time", required = true)]
    pub resolved_at: Option<String>,
    pub origin: Origin,
    #[schema(required = true)]
    pub source_ref: Option<String>,
    #[schema(required = true)]
    pub failure: Option<cannery_row__reviews__routes__FailureOut>,
    #[schema(required = true)]
    pub evaluation: Option<ReadEvidenceEnvelope>,
    pub decisions: Vec<DecisionOut>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum cannery_row__search__routes__Kind {
    #[serde(rename = "track")]
    Track,
    #[serde(rename = "hypothesis")]
    Hypothesis,
    #[serde(rename = "attempt")]
    Attempt,
    #[serde(rename = "report")]
    Report,
    #[serde(rename = "tester_observation")]
    TesterObservation,
    #[serde(rename = "evaluator_reason")]
    EvaluatorReason,
    #[serde(rename = "decision_reason")]
    DecisionReason,
    #[serde(rename = "comment")]
    Comment,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ValidationErrorLocItemValue {
    Variant0(String),
    Variant1(i64),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum StepRefRevisionValue {
    Variant0(i64),
    Variant1(String),
}

/// The public domain-error envelope used by authentication and REST validation.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
}

/// A transport error produced before the domain controller receives the body.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct HttpDetail {
    pub detail: String,
}

#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum BadRequestResponse {
    Domain(ErrorResponse),
    Transport(HttpDetail),
}

// Fixed published request envelopes. Optional fields reject explicit null where
// the domain schemas permit omission but not null; no defaults enter hashes.
fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftReviewRequest {
    #[schema(value_type = i64)]
    pub draft_revision: serde_json::Number,
    pub action: DraftReviewRequestAction,
    pub reason: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum DraftReviewRequestAction {
    #[serde(rename = "approve")]
    Approve,
    #[serde(rename = "request_revision")]
    RequestRevision,
    #[serde(rename = "decline")]
    Decline,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanDecisionRequest {
    pub review_case_id: String,
    pub evidence_revision: i64,
    pub action: HumanDecisionRequestAction,
    pub reason: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub supersedes: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum HumanDecisionRequestAction {
    #[serde(rename = "approve")]
    Approve,
    #[serde(rename = "request_revision")]
    RequestRevision,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "promote")]
    Promote,
    #[serde(rename = "reject")]
    Reject,
    #[serde(rename = "inconclusive")]
    Inconclusive,
    #[serde(rename = "retry")]
    Retry,
    #[serde(rename = "close_failed")]
    CloseFailed,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackTransitionRequest {
    pub to_state: TrackTransitionRequestToState,
    #[schema(value_type = i64)]
    pub expected_revision: serde_json::Number,
    pub reason: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum TrackTransitionRequestToState {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "archived")]
    Archived,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifestRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub attempt_id: String,
    pub objects: Vec<RequestArtifactManifestObject>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestCommonSchemaVersion {
    #[serde(rename = "0.2")]
    Value0,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestArtifactManifestObject {
    pub role: String,
    pub storage: RequestArtifactManifestObjectStorage,
    #[schema(value_type = i64)]
    pub size_bytes: serde_json::Number,
    pub sha256: String,
    pub media_type: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestArtifactManifestObjectStorage {
    pub backend: RequestArtifactManifestObjectStorageBackend,
    pub bucket: String,
    pub key: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub generation: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestArtifactManifestObjectStorageBackend {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "s3")]
    S3,
    #[serde(rename = "gcs")]
    Gcs,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobCompletionRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub job_id: String,
    pub evidence: CompletionEvidenceEnvelopeRequest,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub manifest: Option<ArtifactManifestRequest>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEnvelopeRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub attempt_id: String,
    pub stage: EvidenceEnvelopeRequestStage,
    pub status: EvidenceEnvelopeRequestStatus,
    pub producer: EvidenceEnvelopeRequestProducer,
    #[schema(format = "date-time")]
    pub started_at: String,
    #[schema(format = "date-time")]
    pub finished_at: String,
    pub provenance: EvidenceEnvelopeRequestProvenance,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub observations: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub measurements: Option<Vec<RequestEvidenceEnvelopeMeasurement>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub discrepancies: Option<Vec<RequestEvidenceEnvelopeDiscrepancy>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub artifact_roles: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub report: Option<RequestEvidenceEnvelopeReport>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub manifest: Option<RequestCommonContentRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub assessment: Option<RequestEvidenceEnvelopeAssessment>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub extensions: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum EvidenceEnvelopeRequestStage {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "tester")]
    Tester,
    #[serde(rename = "evaluator")]
    Evaluator,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum EvidenceEnvelopeRequestStatus {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEnvelopeRequestProducer {
    pub kind: EvidenceEnvelopeRequestProducerKind,
    pub id: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum EvidenceEnvelopeRequestProducerKind {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "service")]
    Service,
    #[serde(rename = "builtin")]
    Builtin,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEnvelopeRequestProvenance {
    pub source_revision: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub tester_revision: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub dataset_revision: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub control_revision: Option<String>,
    pub science_revision: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub seed: Option<serde_json::Number>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeMeasurement {
    pub metric: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<f64>)]
    pub value: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub missing_reason: Option<String>,
    pub authority: RequestEvidenceEnvelopeMeasurementAuthority,
    pub unit: String,
    pub direction: RequestEvidenceEnvelopeMeasurementDirection,
    pub split: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub dimensions: Option<BTreeMap<String, String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub sample_count: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<f64>)]
    pub control_value: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub uncertainty: Option<RequestEvidenceEnvelopeMeasurementUncertainty>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeMeasurementAuthority {
    #[serde(rename = "agent_claim")]
    AgentClaim,
    #[serde(rename = "tester_verified")]
    TesterVerified,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeMeasurementDirection {
    #[serde(rename = "higher")]
    Higher,
    #[serde(rename = "lower")]
    Lower,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeMeasurementUncertainty {
    pub method: String,
    #[schema(value_type = f64)]
    pub lower: serde_json::Number,
    #[schema(value_type = f64)]
    pub upper: serde_json::Number,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeDiscrepancy {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub metric: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub split: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub dimensions: Option<BTreeMap<String, String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<f64>)]
    pub claimed_value: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<f64>)]
    pub verified_value: Option<serde_json::Number>,
    pub description: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeReport {
    pub what_was_tried: String,
    pub configuration: String,
    pub observations: String,
    pub findings: String,
    pub limitations: String,
    pub next_question: String,
    #[schema(value_type = f64)]
    pub elapsed_seconds: serde_json::Number,
    pub body_markdown: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestCommonContentRef {
    pub r#ref: String,
    pub sha256: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeAssessment {
    pub policy_revision: String,
    pub gates: Vec<RequestEvidenceEnvelopeAssessmentGatesItem>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub comparisons: Option<Vec<RequestEvidenceEnvelopeComparison>>,
    pub evidence: Vec<RequestCommonContentRef>,
    pub verdict: RequestEvidenceEnvelopeAssessmentVerdict,
    pub reason: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeAssessmentGatesItem {
    pub id: String,
    pub result: RequestEvidenceEnvelopeAssessmentGatesItemResult,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub detail: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeAssessmentGatesItemResult {
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "fail")]
    Fail,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeComparison {
    pub metric: String,
    pub split: String,
    pub dimensions: BTreeMap<String, String>,
    #[schema(value_type = f64)]
    pub value: serde_json::Number,
    pub source: RequestEvidenceEnvelopeComparisonSource,
    pub reference: RequestEvidenceEnvelopeComparisonReference,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeComparisonSource {
    #[serde(rename = "tester")]
    Tester,
    #[serde(rename = "evaluator")]
    Evaluator,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidenceEnvelopeComparisonReference {
    #[schema(value_type = f64)]
    pub value: serde_json::Number,
    pub label: String,
    pub kind: RequestEvidenceEnvelopeComparisonReferenceKind,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub r#ref: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeComparisonReferenceKind {
    #[serde(rename = "paper")]
    Paper,
    #[serde(rename = "benchmark")]
    Benchmark,
    #[serde(rename = "promoted_attempt")]
    PromotedAttempt,
    #[serde(rename = "baseline")]
    Baseline,
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "other")]
    Other,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestEvidenceEnvelopeAssessmentVerdict {
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "fail")]
    Fail,
    #[serde(rename = "inconclusive")]
    Inconclusive,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobFailureRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub job_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub step: Option<String>,
    pub error_code: String,
    pub reason: String,
    pub logs: Vec<JobFailureRequestLogsItem>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobFailureRequestLogsItem {
    pub key: String,
    #[schema(value_type = i64)]
    pub size_bytes: serde_json::Number,
    pub sha256: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreateRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub track: String,
    pub title: String,
    pub question: String,
    pub rationale: String,
    pub intervention: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub control: Option<HypothesisCreateRequestControl>,
    pub plan: HypothesisCreateRequestPlan,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub relations: Option<Vec<HypothesisCreateRequestRelationsItem>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub project_fields: Option<BTreeMap<String, serde_json::Value>>,
}

/// Stored historical hypotheses can predate the full publication document.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum HypothesisDocument {
    Native(Box<HypothesisCreateRequest>),
    Legacy(LegacyHypothesisDocument),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyHypothesisDocument {
    pub schema_version: RequestCommonSchemaVersion,
    pub track: String,
    pub title: String,
    pub question: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub relations: Option<Vec<HypothesisCreateRequestRelationsItem>>,
}

/// Evidence reads include provenance that historical import records preserve.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadMeasurement {
    pub metric: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false, value_type = Option<f64>)]
    pub value: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub missing_reason: Option<String>,
    pub authority: ReadMeasurementAuthority,
    pub unit: String,
    pub direction: RequestEvidenceEnvelopeMeasurementDirection,
    pub split: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub dimensions: Option<BTreeMap<String, String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false, value_type = Option<i64>)]
    pub sample_count: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false, value_type = Option<f64>)]
    pub control_value: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub uncertainty: Option<RequestEvidenceEnvelopeMeasurementUncertainty>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub source: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadMeasurementAuthority {
    AgentClaim,
    TesterVerified,
    ImportedArtifact,
    ImportedTranscribed,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreateRequestControl {
    pub kind: HypothesisCreateRequestControlKind,
    pub id: String,
    pub revision: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum HypothesisCreateRequestControlKind {
    #[serde(rename = "baseline")]
    Baseline,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreateRequestPlan {
    pub selection_splits: Vec<String>,
    pub confirmation_splits: Vec<String>,
    pub primary_metric: String,
    pub required_slices: Vec<String>,
    pub success_criteria: String,
    pub falsification_criteria: String,
    pub regression_gates: Vec<String>,
    #[schema(value_type = BTreeMap<String, f64>)]
    pub compute_budget: BTreeMap<String, serde_json::Number>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreateRequestRelationsItem {
    pub kind: HypothesisCreateRequestRelationsItemKind,
    pub hypothesis: HypothesisCreateRequestRelationsItemHypothesis,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum HypothesisCreateRequestRelationsItemKind {
    #[serde(rename = "derived_from")]
    DerivedFrom,
    #[serde(rename = "supersedes")]
    Supersedes,
    #[serde(rename = "related_to")]
    RelatedTo,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum HypothesisCreateRequestRelationsItemHypothesis {
    Variant0(i64),
    Variant1(HypothesisCreateRequestRelationsItemHypothesisVariant1),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HypothesisCreateRequestRelationsItemHypothesisVariant1 {
    pub project: String,
    #[schema(value_type = i64)]
    pub number: serde_json::Number,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackCreateRequest {
    pub slug: String,
    pub title: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub description: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub producer: Option<RequestCommonProducerRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub mode: Option<TrackCreateRequestMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub workflow: Option<TrackCreateRequestWorkflow>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestCommonProducerRef {
    pub name: String,
    #[schema(value_type = i64)]
    pub revision: serde_json::Number,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum TrackCreateRequestMode {
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "workflow")]
    Workflow,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackCreateRequestWorkflow {
    pub steps: Vec<RequestCommonProducerRef>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequest {
    #[serde(rename = "apiVersion")]
    pub api_version: StepManifestRequestApiVersion,
    pub kind: StepManifestRequestKind,
    pub metadata: StepManifestRequestMetadata,
    pub spec: StepManifestRequestSpec,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum StepManifestRequestApiVersion {
    #[serde(rename = "cannery-row/v1")]
    CanneryRowV1,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum StepManifestRequestKind {
    #[serde(rename = "Step")]
    Step,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestMetadata {
    pub name: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpec {
    pub role: StepManifestRequestSpecRole,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub code: Option<StepManifestRequestSpecCode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub setup: Option<StepManifestRequestSpecSetup>,
    pub container: StepManifestRequestSpecContainer,
    #[serde(rename = "activeDeadlineSeconds")]
    #[schema(value_type = i64)]
    pub active_deadline_seconds: serde_json::Number,
    pub network: RequestStepManifestNetwork,
    pub sandbox: String,
    pub inputs: StepManifestRequestSpecInputs,
    pub outputs: StepManifestRequestSpecOutputs,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum StepManifestRequestSpecRole {
    #[serde(rename = "producer")]
    Producer,
    #[serde(rename = "scorer")]
    Scorer,
    #[serde(rename = "validator")]
    Validator,
    #[serde(rename = "experiment")]
    Experiment,
    #[serde(rename = "evaluator")]
    Evaluator,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecCode {
    pub repo: String,
    pub commit: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub path: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecSetup {
    pub run: String,
    pub cache: StepManifestRequestSpecSetupCache,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub network: Option<RequestStepManifestNetwork>,
    #[serde(rename = "activeDeadlineSeconds")]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub active_deadline_seconds: Option<serde_json::Number>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecSetupCache {
    pub key_files: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub paths: Option<Vec<String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum RequestStepManifestNetwork {
    Variant0(RequestStepManifestNetworkVariant0),
    Variant1(RequestStepManifestNetworkVariant1),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestStepManifestNetworkVariant0 {
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestStepManifestNetworkVariant1 {
    pub egress: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecContainer {
    pub image: String,
    pub command: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub args: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub env: Option<Vec<StepManifestRequestSpecContainerEnvItem>>,
    pub resources: StepManifestRequestSpecContainerResources,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecContainerEnvItem {
    pub name: String,
    pub value: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecContainerResources {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub requests: Option<BTreeMap<String, String>>,
    pub limits: BTreeMap<String, String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecInputs {
    pub artifacts: Vec<StepManifestRequestSpecInputsArtifactsItem>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecInputsArtifactsItem {
    pub name: String,
    pub r#from: StepManifestRequestSpecInputsArtifactsItemFrom,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub interface: Option<String>,
    pub path: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum StepManifestRequestSpecInputsArtifactsItemFrom {
    #[serde(rename = "attempt")]
    Attempt,
    #[serde(rename = "dataset")]
    Dataset,
    #[serde(rename = "baseline")]
    Baseline,
    #[serde(rename = "step")]
    Step,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecOutputs {
    pub artifacts: Vec<StepManifestRequestSpecOutputsArtifactsItem>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepManifestRequestSpecOutputsArtifactsItem {
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub interface: Option<String>,
    pub path: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequest {
    pub schema_version: RequestCommonSchemaVersion,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub hypothesis_fields: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub result_extensions: Option<BTreeMap<String, serde_json::Value>>,
    pub tester: ScienceRevisionRequestTester,
    pub metrics: Vec<RequestScienceRevisionMetric>,
    pub datasets: Vec<ScienceRevisionRequestDatasetsItem>,
    pub baselines: Vec<ScienceRevisionRequestBaselinesItem>,
    pub interfaces: Vec<InterfaceRequest>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub validators: Option<Vec<StepManifestRequest>>,
    pub scorer: StepManifestRequest,
    pub default_producer: RequestCommonProducerRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub code_repositories: Option<ScienceRevisionRequestCodeRepositories>,
    pub evaluator: ScienceRevisionRequestEvaluator,
    pub required_artifact_roles: ScienceRevisionRequestRequiredArtifactRoles,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub retention: Option<Vec<ScienceRevisionRequestRetentionItem>>,
    pub limits: ScienceRevisionRequestLimits,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub max_auto_retries: Option<serde_json::Number>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ConfigDocument {
    Native(ConfigRevisionRequest),
    LegacyScience(Box<LegacyScienceRevision>),
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyScienceRevision {
    pub schema_version: RequestCommonSchemaVersion,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub hypothesis_fields: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub result_extensions: Option<BTreeMap<String, serde_json::Value>>,
    pub tester: ScienceRevisionRequestTester,
    pub metrics: Vec<RequestScienceRevisionMetric>,
    pub datasets: Vec<ScienceRevisionRequestDatasetsItem>,
    pub baselines: Vec<ScienceRevisionRequestBaselinesItem>,
    pub interfaces: Vec<InterfaceRequest>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub validators: Option<Vec<StepManifestRequest>>,
    pub scorer: StepManifestRequest,
    pub default_producer: RequestCommonProducerRef,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub code_repositories: Option<ScienceRevisionRequestCodeRepositories>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub gates: Option<Vec<RequestCommonGate>>,
    pub required_artifact_roles: ScienceRevisionRequestRequiredArtifactRoles,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub retention: Option<Vec<ScienceRevisionRequestRetentionItem>>,
    pub limits: ScienceRevisionRequestLimits,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub max_auto_retries: Option<serde_json::Number>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestCommonGate {
    pub id: String,
    pub metric: String,
    pub split: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub per_dimension: Option<String>,
    pub statistic: GateStatistic,
    pub compare: GateComparison,
    pub op: GateOperator,
    #[schema(value_type = f64)]
    pub min_delta: serde_json::Number,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum GateStatistic {
    #[serde(rename = "value")]
    Value,
    #[serde(rename = "uncertainty.lower")]
    UncertaintyLower,
    #[serde(rename = "uncertainty.upper")]
    UncertaintyUpper,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum GateComparison {
    #[serde(rename = "control")]
    Control,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum GateOperator {
    #[serde(rename = ">")]
    Greater,
    #[serde(rename = ">=")]
    GreaterOrEqual,
    #[serde(rename = "<")]
    Less,
    #[serde(rename = "<=")]
    LessOrEqual,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ReadEvidenceEnvelope {
    Native(Box<EvidenceEnvelopeRequest>),
    Imported(Box<ImportedEvidenceEnvelope>),
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedEvidenceEnvelope {
    pub schema_version: RequestCommonSchemaVersion,
    pub attempt_id: String,
    pub stage: EvidenceEnvelopeRequestStage,
    pub status: EvidenceEnvelopeRequestStatus,
    pub producer: ImportedEvidenceProducer,
    #[schema(format = "date-time")]
    pub started_at: String,
    #[schema(format = "date-time")]
    pub finished_at: String,
    pub provenance: ImportedEvidenceProvenance,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub observations: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub measurements: Option<Vec<ReadMeasurement>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub assessment: Option<RequestEvidenceEnvelopeAssessment>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedEvidenceProducer {
    pub kind: ImportedEvidenceProducerKind,
    pub id: ImportedEvidenceProducerId,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum ImportedEvidenceProducerKind {
    #[serde(rename = "import")]
    Import,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum ImportedEvidenceProducerId {
    #[serde(rename = "cannery-import")]
    CanneryImport,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedEvidenceProvenance {
    pub science_revision: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub source_revision: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestTester {
    pub id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub revision: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestScienceRevisionMetric {
    pub key: String,
    pub unit: String,
    pub direction: RequestScienceRevisionMetricDirection,
    pub aggregation: RequestScienceRevisionMetricAggregation,
    pub dimensions: Vec<RequestScienceRevisionMetricDimensionsItem>,
    pub splits: Vec<String>,
    pub required_slices: Vec<RequestScienceRevisionMetricRequiredSlicesItem>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestScienceRevisionMetricDirection {
    #[serde(rename = "higher")]
    Higher,
    #[serde(rename = "lower")]
    Lower,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestScienceRevisionMetricAggregation {
    #[serde(rename = "mean")]
    Mean,
    #[serde(rename = "median")]
    Median,
    #[serde(rename = "sum")]
    Sum,
    #[serde(rename = "min")]
    Min,
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "count")]
    Count,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestScienceRevisionMetricDimensionsItem {
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub values: Option<Vec<String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestScienceRevisionMetricRequiredSlicesItem {
    pub dimension: String,
    pub values: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub splits: Option<Vec<String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestDatasetsItem {
    pub id: String,
    pub revision: String,
    pub held_out_labels: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestBaselinesItem {
    pub id: String,
    pub revision: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub description: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InterfaceRequest {
    pub name: String,
    #[schema(value_type = i64)]
    pub version: serde_json::Number,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub schema: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub format: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub media_type: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub encoding: Option<InterfaceRequestEncoding>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub max_bytes: Option<serde_json::Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub allow_empty: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub magic: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub validate: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub validator: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum InterfaceRequestEncoding {
    #[serde(rename = "json")]
    Json,
    #[serde(rename = "jsonl")]
    Jsonl,
    #[serde(rename = "binary")]
    Binary,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestCodeRepositories {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub candidate: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub trusted: Option<Vec<String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestEvaluator {
    pub id: String,
    pub revision: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestRequiredArtifactRoles {
    pub attempt: Vec<String>,
    pub tester: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestRetentionItem {
    pub role: String,
    #[schema(value_type = i64)]
    pub days: serde_json::Number,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScienceRevisionRequestLimits {
    pub resource_ceilings: BTreeMap<String, String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    #[schema(value_type = Option<i64>)]
    pub max_deadline_seconds: Option<serde_json::Number>,
    #[schema(value_type = i64)]
    pub report_max_bytes: serde_json::Number,
    #[schema(value_type = i64)]
    pub max_output_bytes: serde_json::Number,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DashboardRevisionRequest {
    pub views: Vec<RequestDashboardViewsView>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestDashboardViewsView {
    pub id: String,
    pub title: String,
    pub chart: RequestDashboardViewsViewChart,
    pub metric: String,
    pub split: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub x: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub group_by: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub baseline: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub filters: Option<BTreeMap<String, Vec<String>>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum RequestDashboardViewsViewChart {
    #[serde(rename = "line")]
    Line,
    #[serde(rename = "scatter")]
    Scatter,
    #[serde(rename = "bar")]
    Bar,
    #[serde(rename = "table")]
    Table,
}

/// The config-kind path selects the matching published revision contract.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ConfigRevisionRequest {
    Science(Box<ScienceRevisionRequest>),
    Dashboard(DashboardRevisionRequest),
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompletionEvidenceEnvelopeRequest {
    pub schema_version: RequestCommonSchemaVersion,
    pub attempt_id: String,
    pub stage: CompletionEvidenceStage,
    pub status: EvidenceEnvelopeRequestStatus,
    pub producer: EvidenceEnvelopeRequestProducer,
    #[schema(format = "date-time")]
    pub started_at: String,
    #[schema(format = "date-time")]
    pub finished_at: String,
    pub provenance: EvidenceEnvelopeRequestProvenance,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub observations: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub measurements: Option<Vec<RequestEvidenceEnvelopeMeasurement>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub discrepancies: Option<Vec<RequestEvidenceEnvelopeDiscrepancy>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub artifact_roles: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub report: Option<RequestEvidenceEnvelopeReport>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub manifest: Option<RequestCommonContentRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub assessment: Option<RequestEvidenceEnvelopeAssessment>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_non_null"
    )]
    #[schema(nullable = false)]
    pub extensions: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum CompletionEvidenceStage {
    #[serde(rename = "tester")]
    Tester,
    #[serde(rename = "evaluator")]
    Evaluator,
}

/// Decode the declared request DTO after authorization/domain checks, then pass
/// its serde representation to the controller while retaining field presence.
/// In particular, invalid leased completions still take the durable failure path.
pub(crate) fn request_document<T>(
    document: &cannery_core::json::Document,
) -> Result<cannery_core::json::Document, crate::errors::ApiError>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let invalid = || {
        crate::errors::ApiError::from(cannery_core::errors::DomainError::new(
            cannery_core::errors::ErrorCode::ValidationFailed,
            "request does not match the REST contract",
        ))
    };
    let mut builder = cannery_core::json::DocumentBuilder::new();
    let root = builder
        .import(document, document.root())
        .map_err(|_| invalid())?;
    let copied = builder.finish(root).map_err(|_| invalid())?;
    let body = crate::api_contract::typed_body::<T>(crate::body::DecodedBody::Json(copied))?;
    match body {
        crate::body::DecodedBody::Json(document) => Ok(document),
        _ => Err(crate::errors::ApiError::from(
            cannery_core::errors::DomainError::new(
                cannery_core::errors::ErrorCode::ValidationFailed,
                "request does not match the REST contract",
            ),
        )),
    }
}

#[cfg(test)]
mod request_tests {
    use super::{EvidenceEnvelopeRequest, JobFailureRequest, request_document};
    use cannery_core::json;
    use serde_json::{Value, json as value};

    #[test]
    fn accepted_adapter_preserves_nested_omission_and_dynamic_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut source: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
        ))?;
        source["extensions"] = value!({"custom":[null,false,{"label":"é𐀀"}]});
        let document = json::decode(&serde_json::to_vec(&source)?, 128)?;
        let converted = request_document::<EvidenceEnvelopeRequest>(&document)?;
        assert_eq!(json::to_value(&converted)?, source);
        assert!(converted.field(converted.root(), "observations").is_none());
        Ok(())
    }

    #[test]
    fn accepted_adapter_errors_never_echo_private_values() -> Result<(), Box<dyn std::error::Error>>
    {
        let document = json::decode(br#"{"schema_version":"0.2","job_id":"private-marker","error_code":"step_error","reason":"private-marker","logs":{},"unexpected":"private-marker"}"#, 128)?;
        let error = request_document::<JobFailureRequest>(&document)
            .err()
            .ok_or("expected rejection")?;
        assert!(!format!("{error:?}").contains("private-marker"));
        Ok(())
    }
}

/// Fixed published job contract, including its claimed lease and pinned inputs.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobDocument {
    pub schema_version: RequestCommonSchemaVersion,
    pub job_id: String,
    pub stage: CompletionEvidenceStage,
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tester: Option<ClaimedJobService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<ClaimedJobService>,
    pub track: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ClaimedJobPinnedRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Frozen project fields can contain scalar legacy values.
    pub parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<ClaimedJobStep>>,
    pub science_revision: String,
    pub inputs: ClaimedJobInputs,
    pub output_prefix: String,
    #[schema(format = "date-time")]
    pub deadline: String,
    pub limits: ClaimedJobLimits,
    pub lease: ClaimedJobLease,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobService {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobPinnedRef {
    pub id: String,
    pub revision: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobStep {
    pub name: String,
    pub revision: StepRefRevisionValue,
    pub manifest: StepManifestRequest,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_sheet: Option<RequestCommonContentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<RequestCommonContentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<RequestCommonContentRef>>,
    pub baselines: Vec<ClaimedJobPinnedRef>,
    pub datasets: Vec<ClaimedJobPinnedRef>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobLimits {
    pub max_output_bytes: i64,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedJobLease {
    pub token: String,
    pub generation: i64,
    #[schema(format = "date-time")]
    pub expires_at: String,
}
/// Native agent report or a separately represented legacy imported report.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum ReportDocument {
    Native(RequestEvidenceEnvelopeReport),
    Imported(ImportedReportDocument),
    Absent(EmptyReportDocument),
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedReportDocument {
    pub kind: String,
    pub author: String,
    pub written_at: Option<String>,
    pub body_markdown: String,
    pub origin: Origin,
    pub source_ref: Option<String>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyReportDocument {}

/// Resolved workflow with pinned manifests, inputs and the prior attempt.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedWorkflow {
    pub attempt_id: String,
    pub attempt_ref: String,
    pub track: String,
    pub science_revision: String,
    pub steps: Vec<ClaimedJobStep>,
    /// Frozen project fields can contain scalar legacy values.
    pub parameters: serde_json::Value,
    pub inputs: ClaimedWorkflowInputs,
    pub limits: ClaimedJobLimits,
    #[schema(format = "date-time")]
    pub deadline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ClaimedJobPinnedRef>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedWorkflowInputs {
    pub datasets: Vec<ClaimedJobPinnedRef>,
    pub baselines: Vec<ClaimedJobPinnedRef>,
    pub predecessor: Option<ClaimedWorkflowPredecessor>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedWorkflowPredecessor {
    pub attempt_id: String,
    pub r#ref: String,
    pub state: String,
    pub failure_code: Option<String>,
    pub artifacts: Vec<ClaimedWorkflowArtifact>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimedWorkflowArtifact {
    pub id: String,
    pub role: String,
    pub storage: RequestArtifactManifestObjectStorage,
    pub size_bytes: i64,
    pub sha256: String,
    pub media_type: String,
}

#[cfg(test)]
mod nested_contract_tests {
    use super::*;
    use serde_json::{Value, json};

    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source: Value = serde_json::from_slice(bytes)?;
        let typed: T = serde_json::from_value(source.clone())?;
        assert_eq!(serde_json::to_value(typed)?, source);
        Ok(())
    }

    #[test]
    fn imported_example_hypotheses_preserve_their_normalized_read_documents()
    -> Result<(), Box<dyn std::error::Error>> {
        // The importer synthesizes these four fields for histories without a full document.
        for example in [
            include_str!("../../../examples/import/hypotheses/H-001.yaml"),
            include_str!("../../../examples/import/hypotheses/H-002.yaml"),
            include_str!("../../../examples/import/hypotheses/H-003.yaml"),
            include_str!("../../../examples/import/hypotheses/H-004.yaml"),
            include_str!("../../../examples/import/hypotheses/H-005.yaml"),
            include_str!("../../../examples/import/hypotheses/H-006.yaml"),
        ] {
            let header = |field: &str| {
                example
                    .lines()
                    .find_map(|line| line.strip_prefix(&format!("{field}: ")))
                    .ok_or("import example header absent")
            };
            let document = json!({"schema_version":"0.2", "track":header("track")?,
                "title":header("title")?, "question":header("claim")?});
            let typed: HypothesisDocument = serde_json::from_value(document.clone())?;
            assert!(matches!(typed, HypothesisDocument::Legacy(_)));
            assert_eq!(serde_json::to_value(typed)?, document);
            assert!(serde_json::from_value::<HypothesisCreateRequest>(document.clone()).is_err());
            let revision = json!({"revision":1,"science_revision":1,
                "author":{"kind":"user","id":"00000000-0000-0000-0000-000000000001"},
                "via_channel":"cli","via_client":"cannery import",
                "created_at":"2025-01-08T00:00:00Z","origin":"imported","document":document});
            round_trip::<RevisionOut>(&serde_json::to_vec(&revision)?)?;
            let mut malformed = document;
            malformed["plan"] = json!("invalid native plan");
            assert!(serde_json::from_value::<HypothesisDocument>(malformed).is_err());
        }
        let full = include_bytes!("../../../tests/fixtures/contracts/hypothesis/valid/full.json");
        let native: HypothesisDocument = serde_json::from_slice(full)?;
        assert!(matches!(native, HypothesisDocument::Native(_)));
        round_trip::<HypothesisDocument>(full)?;
        Ok(())
    }

    #[test]
    fn imported_measurements_preserve_authority_source_and_missing_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let example = include_str!("../../../examples/import/hypotheses/H-001.yaml");
        let source = "gs://retrieval-history/runs/h001-seed-1/metrics.json#/mrr/overall";
        assert!(example.contains(source));
        let measurement = json!({"metric":"mrr","split":"dev","value":0.71,
            "control_value":0.68,"sample_count":200,"authority":"imported_artifact",
            "source":source,"unit":"ratio","direction":"higher","dimensions":{}});
        round_trip::<ReadMeasurement>(&serde_json::to_vec(&measurement)?)?;
        assert!(serde_json::from_value::<RequestEvidenceEnvelopeMeasurement>(measurement).is_err());
        let missing = json!({"metric":"mrr","split":"dev","dimensions":{"language":"en"},
            "missing_reason":"The English slice was not scored in this run.",
            "authority":"imported_transcribed","source":"notebook/2025-02.md:14@3f9c2ab",
            "unit":"ratio","direction":"higher"});
        let example = include_str!("../../../examples/import/hypotheses/H-003.yaml");
        assert!(example.contains(missing["source"].as_str().ok_or("source absent")?));
        round_trip::<ReadMeasurement>(&serde_json::to_vec(&missing)?)?;
        let mut malformed = missing;
        malformed["authority"] = json!("unknown_authority");
        assert!(serde_json::from_value::<ReadMeasurement>(malformed).is_err());
        Ok(())
    }

    #[test]
    fn imported_evidence_preserves_writer_provenance_and_fixed_producer()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = json!({"schema_version":"0.2",
            "attempt_id":"00000000-0000-0000-0000-000000000041","stage":"evaluator",
            "status":"completed","producer":{"kind":"import","id":"cannery-import"},
            "provenance":{"science_revision":"1"},"started_at":"2025-03-05T00:00:00Z",
            "finished_at":"2025-03-05T00:00:00Z","assessment":{"policy_revision":"release-gate@fixture-r1",
            "gates":[{"id":"mrr-holds","result":"pass"}],"evidence":[],"verdict":"pass",
            "reason":"Both gates pass; the gain over H-001 is 0.02."}});
        let example = include_str!("../../../examples/import/hypotheses/H-006.yaml");
        assert!(
            example.contains(
                envelope["assessment"]["reason"]
                    .as_str()
                    .ok_or("reason absent")?
            )
        );
        round_trip::<ReadEvidenceEnvelope>(&serde_json::to_vec(&envelope)?)?;
        assert!(serde_json::from_value::<EvidenceEnvelopeRequest>(envelope.clone()).is_err());
        envelope["provenance"]["source_revision"] = json!("8d1e0f4");
        round_trip::<ReadEvidenceEnvelope>(&serde_json::to_vec(&envelope)?)?;
        envelope["provenance"]
            .as_object_mut()
            .ok_or("provenance absent")?
            .remove("source_revision");
        envelope["producer"]["kind"] = json!("agent");
        assert!(serde_json::from_value::<ReadEvidenceEnvelope>(envelope).is_err());
        round_trip::<ReadEvidenceEnvelope>(include_bytes!(
            "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
        ))?;
        Ok(())
    }

    #[test]
    fn historical_science_reads_without_evaluator_preserve_typed_gates()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = include_bytes!("../../../examples/fixture/science.json");
        let native: Value = serde_json::from_slice(fixture)?;
        assert!(serde_json::from_value::<LegacyScienceRevision>(native).is_err());
        let mut historical: Value = serde_json::from_slice(fixture)?;
        historical
            .as_object_mut()
            .ok_or("science object absent")?
            .remove("evaluator");
        round_trip::<ConfigDocument>(&serde_json::to_vec(&historical)?)?;
        assert!(serde_json::from_value::<ConfigRevisionRequest>(historical.clone()).is_err());
        historical["gates"] = json!([{"id":"g","metric":"mrr","split":"dev",
            "statistic":"value","compare":"control","op":">=","min_delta":0.0}]);
        round_trip::<ConfigDocument>(&serde_json::to_vec(&historical)?)?;
        historical["gates"][0]["op"] = json!("arbitrary_expression");
        assert!(serde_json::from_value::<ConfigDocument>(historical).is_err());
        round_trip::<ConfigDocument>(fixture)?;
        Ok(())
    }

    #[test]
    fn leased_jobs_preserve_pinned_inputs_manifests_and_optional_services()
    -> Result<(), Box<dyn std::error::Error>> {
        for bytes in [
            include_bytes!("../../../tests/fixtures/contracts/job/valid/tester_self_hosted.json")
                .as_slice(),
            include_bytes!("../../../tests/fixtures/contracts/job/valid/tester_runner.json")
                .as_slice(),
            include_bytes!("../../../tests/fixtures/contracts/job/valid/evaluator.json").as_slice(),
        ] {
            round_trip::<ClaimedJobDocument>(bytes)?;
            let mut malformed: Value = serde_json::from_slice(bytes)?;
            malformed["lease"]["generation"] = json!("1");
            assert!(serde_json::from_value::<ClaimedJobDocument>(malformed).is_err());
        }
        Ok(())
    }

    #[test]
    fn resolved_workflow_uses_pinned_control_and_preserves_predecessor_and_parameters()
    -> Result<(), Box<dyn std::error::Error>> {
        let job: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/contracts/job/valid/tester_runner.json"
        ))?;
        let workflow = json!({"attempt_id":"attempt", "attempt_ref":"#1.1", "track":"track", "science_revision":"1", "steps":job["steps"], "parameters":{"custom":[null,true]}, "inputs":{"datasets":[], "baselines":[], "predecessor":null}, "limits":{"max_output_bytes":1024}, "deadline":"2026-10-06T00:00:00Z", "control":{"id":"control", "revision":"1"}});
        round_trip::<ClaimedWorkflow>(&serde_json::to_vec(&workflow)?)?;
        Ok(())
    }

    #[test]
    fn nested_publication_and_report_contracts_preserve_valid_documents()
    -> Result<(), Box<dyn std::error::Error>> {
        round_trip::<HypothesisCreateRequest>(include_bytes!(
            "../../../tests/fixtures/contracts/hypothesis/valid/full.json"
        ))?;
        round_trip::<EvidenceEnvelopeRequest>(include_bytes!(
            "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
        ))?;
        round_trip::<ConfigRevisionRequest>(include_bytes!(
            "../../../tests/fixtures/contracts/science_revision/valid/stock_evaluator.json"
        ))?;
        round_trip::<ConfigRevisionRequest>(include_bytes!(
            "../../../tests/fixtures/contracts/dashboard_views/valid/table_and_facets.json"
        ))?;
        for report in [
            json!({"what_was_tried":"one", "configuration":"two", "observations":"three", "findings":"four", "limitations":"five", "next_question":"six", "elapsed_seconds":1, "body_markdown":"text"}),
            json!({"kind":"report", "author":"legacy", "written_at":null, "body_markdown":"old", "origin":"imported", "source_ref":"legacy:1"}),
            json!({}),
        ] {
            round_trip::<ReportDocument>(&serde_json::to_vec(&report)?)?;
        }
        Ok(())
    }

    #[test]
    fn fixed_nested_request_shapes_reject_generic_objects_and_preserve_patch_null()
    -> Result<(), Box<dyn std::error::Error>> {
        for field in ["producer", "workflow"] {
            let mut patch = json!({"expected_revision":1});
            patch[field] = json!({"unrelated":"private"});
            assert!(serde_json::from_value::<TrackUpdate>(patch).is_err());
        }
        let patch: TrackUpdate =
            serde_json::from_value(json!({"expected_revision":1,"producer":null}))?;
        assert!(patch.producer.is_none());
        assert!(
            serde_json::from_value::<DraftUpdate>(json!({"expected_revision":1,"document":{}}))
                .is_err()
        );
        let mut hypothesis: Value = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/contracts/hypothesis/valid/full.json"
        ))?;
        hypothesis["plan"]["unknown"] = json!(1);
        assert!(
            serde_json::from_value::<DraftUpdate>(
                json!({"expected_revision":1,"document":hypothesis})
            )
            .is_err()
        );
        Ok(())
    }
}
