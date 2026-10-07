use crate::raw::RawAttempt;
use crate::{
    arguments::{Argument, Value},
    model::{ArtifactId, EvidenceId, FailureId, ManifestId, UploadId},
    repo::AttemptError,
    wire::JsonbText,
};
use cannery_core::{
    ids::{
        AttemptId, HypothesisId, JobId, ProjectId, ReviewCaseId, ServiceAccountId, TrackId, UserId,
    },
    timestamps::Timestamp,
};
use sqlx::{
    PgConnection, Postgres,
    postgres::{PgArguments, PgRow},
    query::{Map, Query, QueryScalar},
};

pub(crate) fn get_attempt_lock_query(
    args: &[Argument],
) -> QueryScalar<'_, Postgres, AttemptId, PgArguments> {
    sqlx::query_file_scalar!(
        "src/sql/get_attempt_0.sql",
        &args[0] as _,
        &args[1] as _,
        &args[2] as _
    )
}
pub(crate) fn get_attempt_query(
    args: &[Argument],
) -> Map<'_, Postgres, impl FnMut(PgRow) -> Result<RawAttempt, sqlx::Error> + Send, PgArguments> {
    sqlx::query_file_as!(
        RawAttempt,
        "src/sql/get_attempt_1.sql",
        &args[0] as _,
        &args[1] as _,
        &args[2] as _
    )
}
pub(crate) fn get_attempt_by_id_lock_query(
    args: &[Argument],
) -> QueryScalar<'_, Postgres, i32, PgArguments> {
    sqlx::query_file_scalar!("src/sql/get_attempt_by_id_0.sql", &args[0] as _)
}
pub(crate) fn get_attempt_by_id_query(
    args: &[Argument],
) -> Map<'_, Postgres, impl FnMut(PgRow) -> Result<RawAttempt, sqlx::Error> + Send, PgArguments> {
    sqlx::query_file_as!(RawAttempt, "src/sql/get_attempt_by_id_1.sql", &args[0] as _)
}
pub(crate) fn list_attempts_query(
    args: &[Argument],
) -> Map<'_, Postgres, impl FnMut(PgRow) -> Result<RawAttempt, sqlx::Error> + Send, PgArguments> {
    sqlx::query_file_as!(
        RawAttempt,
        "src/sql/list_attempts_0.sql",
        &args[0] as _,
        &args[1] as _,
        &args[2] as _,
        &args[3] as _
    )
}
pub(crate) fn list_project_attempts_query(
    args: &[Argument],
) -> Map<'_, Postgres, impl FnMut(PgRow) -> Result<RawAttempt, sqlx::Error> + Send, PgArguments> {
    sqlx::query_file_as!(
        RawAttempt,
        "src/sql/list_project_attempts_0.sql",
        &args[0] as _,
        &args[1] as _,
        &args[2] as _,
        &args[3] as _,
        &args[4] as _
    )
}
pub(crate) fn extend_lease_query(args: &[Argument]) -> Query<'_, Postgres, PgArguments> {
    sqlx::query_file!("src/sql/extend_lease_0.sql", &args[0] as _, &args[1] as _)
}
pub(crate) fn mark_running_query(args: &[Argument]) -> Query<'_, Postgres, PgArguments> {
    sqlx::query_file!("src/sql/mark_running_0.sql", &args[0] as _)
}
pub(crate) fn end_lease_query(args: &[Argument]) -> Query<'_, Postgres, PgArguments> {
    sqlx::query_file!(
        "src/sql/end_lease_0.sql",
        &args[0] as _,
        &args[1] as _,
        &args[2] as _,
        &args[3] as _
    )
}
#[derive(Clone, Copy)]
pub(crate) enum Statement {
    PickClaimable0,
    CreateAttempt0,
    GetAttempt0,
    GetAttemptById0,
    ExtendLease0,
    MarkRunning0,
    EndLease0,
    MoveAttempt0,
    ReopenAttempt0,
    PinTrack0,
    ApprovedProjectFields0,
    ApprovedControl0,
    CreateUpload0,
    CreateUpload1,
    CountOpenUploads0,
    GetUpload0,
    GetUpload0Locked,
    BeginReceiving0,
    AbandonReceiving0,
    FinishUpload0,
    ObjectDeleted0,
    RecordUrls0,
    ListRefusedUploads0,
    AddArtifact0,
    ListArtifacts0,
    GetArtifact0,
    GetArtifactByKey0,
    ListArtifactsByRole0,
    ListJobArtifacts0,
    AddManifest0,
    GetManifest0,
    AddEvidence0,
    GetEvidenceById0,
    GetEvidence0,
    RecordFailure0,
    RecordFailure1,
    RecordFailure2,
    RequeueFailed0,
    RequeueFailed1,
    AutomaticRequeues0,
    ListFailures0,
    LastFailureCode0,
}
pub(crate) async fn execute(
    conn: &mut PgConnection,
    statement: Statement,
    values: Vec<Value>,
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    let args_values = values
        .iter()
        .map(Argument::new)
        .collect::<Result<Vec<_>, _>>()?;
    match statement {
        Statement::PickClaimable0 => pick_claimable0(conn, &args_values).await,
        Statement::CreateAttempt0 => create_attempt0(conn, &args_values).await,
        Statement::GetAttempt0 => get_attempt0(conn, &args_values).await,
        Statement::GetAttemptById0 => get_attempt_by_id0(conn, &args_values).await,
        Statement::ExtendLease0 => extend_lease0(conn, &args_values).await,
        Statement::MarkRunning0 => mark_running0(conn, &args_values).await,
        Statement::EndLease0 => end_lease0(conn, &args_values).await,
        Statement::MoveAttempt0 => move_attempt0(conn, &args_values).await,
        Statement::ReopenAttempt0 => reopen_attempt0(conn, &args_values).await,
        Statement::PinTrack0 => pin_track0(conn, &args_values).await,
        Statement::ApprovedProjectFields0 => approved_project_fields0(conn, &args_values).await,
        Statement::ApprovedControl0 => approved_control0(conn, &args_values).await,
        Statement::CreateUpload0 => create_upload0(conn, &args_values).await,
        Statement::CreateUpload1 => {
            let Some(Value::Integer(Some(minutes))) = values.get(16) else {
                return Err(AttemptError::Invariant);
            };
            // PostgreSQL's make_interval minutes parameter is always int4.
            let minutes =
                i32::try_from(minutes).map_err(|_| AttemptError::Database { sqlstate: None })?;
            create_upload1(conn, &args_values, minutes).await
        }
        Statement::CountOpenUploads0 => count_open_uploads0(conn, &args_values).await,
        Statement::GetUpload0 => get_upload0(conn, &args_values).await,
        Statement::GetUpload0Locked => get_upload0_locked(conn, &args_values).await,
        Statement::BeginReceiving0 => begin_receiving0(conn, &args_values).await,
        Statement::AbandonReceiving0 => abandon_receiving0(conn, &args_values).await,
        other => execute_remaining(conn, other, &args_values).await,
    }
}

async fn execute_remaining(
    conn: &mut PgConnection,
    statement: Statement,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    match statement {
        Statement::FinishUpload0 => finish_upload0(conn, args_values).await,
        Statement::ObjectDeleted0 => object_deleted0(conn, args_values).await,
        Statement::RecordUrls0 => record_urls0(conn, args_values).await,
        Statement::ListRefusedUploads0 => list_refused_uploads0(conn, args_values).await,
        Statement::AddArtifact0 => add_artifact0(conn, args_values).await,
        Statement::ListArtifacts0 => list_artifacts0(conn, args_values).await,
        Statement::GetArtifact0 => get_artifact0(conn, args_values).await,
        Statement::GetArtifactByKey0 => get_artifact_by_key0(conn, args_values).await,
        Statement::ListArtifactsByRole0 => list_artifacts_by_role0(conn, args_values).await,
        Statement::ListJobArtifacts0 => list_job_artifacts0(conn, args_values).await,
        Statement::AddManifest0 => add_manifest0(conn, args_values).await,
        Statement::GetManifest0 => get_manifest0(conn, args_values).await,
        Statement::AddEvidence0 => add_evidence0(conn, args_values).await,
        Statement::GetEvidenceById0 => get_evidence_by_id0(conn, args_values).await,
        Statement::GetEvidence0 => get_evidence0(conn, args_values).await,
        Statement::RecordFailure0 => record_failure0(conn, args_values).await,
        Statement::RecordFailure1 => record_failure1(conn, args_values).await,
        Statement::RecordFailure2 => record_failure2(conn, args_values).await,
        Statement::RequeueFailed0 => requeue_failed0(conn, args_values).await,
        Statement::RequeueFailed1 => requeue_failed1(conn, args_values).await,
        Statement::AutomaticRequeues0 => automatic_requeues0(conn, args_values).await,
        Statement::ListFailures0 => list_failures0(conn, args_values).await,
        Statement::LastFailureCode0 => last_failure_code0(conn, args_values).await,
        _ => Err(AttemptError::Invariant),
    }
}

async fn pick_claimable0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 7 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/pick_claimable_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _,
        &args_values[6] as _
    );
    native_results(conn, query).await
}

async fn create_attempt0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 11 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/create_attempt_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _,
        &args_values[6] as _,
        &args_values[7] as _,
        &args_values[8] as _,
        &args_values[9] as _,
        &args_values[10] as _
    );
    native_results(conn, query).await
}

async fn get_attempt0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 3 {
        return Err(AttemptError::Invariant);
    }
    let query = get_attempt_lock_query(args_values);
    native_results(conn, query).await
}

async fn get_attempt_by_id0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = get_attempt_by_id_lock_query(args_values);
    native_results(conn, query).await
}

async fn extend_lease0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = extend_lease_query(args_values);
    native_results(conn, query).await
}

async fn mark_running0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = mark_running_query(args_values);
    native_results(conn, query).await
}

async fn end_lease0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = end_lease_query(args_values);
    native_results(conn, query).await
}

async fn move_attempt0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/move_attempt_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _
    );
    native_results(conn, query).await
}

async fn reopen_attempt0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/reopen_attempt_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn pin_track0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/pin_track_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn approved_project_fields0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/approved_project_fields_0.sql",
        &args_values[0] as _
    );
    native_results(conn, query).await
}

async fn approved_control0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/approved_control_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn create_upload0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/create_upload_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _
    );
    native_results(conn, query).await
}

async fn create_upload1(
    conn: &mut PgConnection,
    args_values: &[Argument],
    ttl_minutes: i32,
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 22 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/create_upload_1.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _,
        &args_values[6] as _,
        &args_values[7] as _,
        &args_values[8] as _,
        &args_values[9] as _,
        &args_values[10] as _,
        &args_values[11] as _,
        &args_values[12] as _,
        &args_values[13] as _,
        &args_values[14] as _,
        &args_values[15] as _,
        ttl_minutes,
        &args_values[17] as _,
        &args_values[18] as _,
        &args_values[19] as _,
        &args_values[20] as _,
        &args_values[21] as _
    );
    native_results(conn, query).await
}

async fn count_open_uploads0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/count_open_uploads_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn get_upload0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/get_upload_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn get_upload0_locked(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/get_upload_0_locked.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn begin_receiving0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/begin_receiving_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn abandon_receiving0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/abandon_receiving_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn finish_upload0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/finish_upload_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _
    );
    native_results(conn, query).await
}

async fn object_deleted0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/object_deleted_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn record_urls0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/record_urls_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn list_refused_uploads0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/list_refused_uploads_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn add_artifact0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 13 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/add_artifact_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _,
        &args_values[6] as _,
        &args_values[7] as _,
        &args_values[8] as _,
        &args_values[9] as _,
        &args_values[10] as _,
        &args_values[11] as _,
        &args_values[12] as _
    );
    native_results(conn, query).await
}

async fn list_artifacts0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/list_artifacts_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn get_artifact0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/get_artifact_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn get_artifact_by_key0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/get_artifact_by_key_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _
    );
    native_results(conn, query).await
}

async fn list_artifacts_by_role0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/list_artifacts_by_role_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn list_job_artifacts0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/list_job_artifacts_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn add_manifest0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 4 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/add_manifest_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _
    );
    native_results(conn, query).await
}

async fn get_manifest0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/get_manifest_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn add_evidence0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 13 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/add_evidence_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _,
        &args_values[6] as _,
        &args_values[7] as _,
        &args_values[8] as _,
        &args_values[9] as _,
        &args_values[10] as _,
        &args_values[11] as _,
        &args_values[12] as _
    );
    native_results(conn, query).await
}

async fn get_evidence_by_id0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/get_evidence_by_id_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn get_evidence0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 2 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/get_evidence_0.sql",
        &args_values[0] as _,
        &args_values[1] as _
    );
    native_results(conn, query).await
}

async fn record_failure0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 6 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/record_failure_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _,
        &args_values[5] as _
    );
    native_results(conn, query).await
}

async fn record_failure1(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/record_failure_1.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn record_failure2(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 5 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/record_failure_2.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _
    );
    native_results(conn, query).await
}

async fn requeue_failed0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 5 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!(
        "src/sql/requeue_failed_0.sql",
        &args_values[0] as _,
        &args_values[1] as _,
        &args_values[2] as _,
        &args_values[3] as _,
        &args_values[4] as _
    );
    native_results(conn, query).await
}

async fn requeue_failed1(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/requeue_failed_1.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn automatic_requeues0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/automatic_requeues_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn list_failures0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/list_failures_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn last_failure_code0(
    conn: &mut PgConnection,
    args_values: &[Argument],
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    if args_values.len() != 1 {
        return Err(AttemptError::Invariant);
    }
    let query = sqlx::query_file!("src/sql/last_failure_code_0.sql", &args_values[0] as _);
    native_results(conn, query).await
}

async fn native_results<'q>(
    conn: &mut PgConnection,
    query: impl sqlx::Execute<'q, Postgres> + 'q,
) -> Result<(u64, Vec<PgRow>), AttemptError> {
    use futures_util::TryStreamExt;
    use sqlx::Executor;
    let results = conn
        .fetch_many(query)
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| AttemptError::database(&error))?;
    let mut count = 0;
    let mut rows = Vec::new();
    for result in results {
        match result {
            sqlx::Either::Left(result) => count += result.rows_affected(),
            sqlx::Either::Right(row) => rows.push(row),
        }
    }
    Ok((count, rows))
}
