//! Who may work on a job: a verifier service account on the runner verify jobs registered to
//! its name, a decider service account on the decide jobs registered to its name, and an agent
//! service account or a researcher on agent jobs (never a verify job of an attempt it ran).
//! Every job route after the claim also requires the caller to hold the job's claim.
use crate::{
    attempt_lease_routes::{Failure, domain, failure},
    requests::RequestContext,
};
use cannery_core::{
    errors::ErrorCode,
    principal::{Principal, Role, ServiceKind},
};
use cannery_jobs::repo::{Claimant, Job, Performer, Phase};
use cannery_projects::{authz, repo::Project};
use sqlx::PgConnection;

/// The project of a caller that may verify: a verifier or agent service account, or a researcher.
pub(crate) async fn worker(
    c: &mut PgConnection,
    p: &Principal,
    slug: &str,
    r: &RequestContext,
) -> Result<Project, Failure> {
    authz::project_access(
        c,
        p,
        slug,
        Some(Role::Researcher),
        &[
            ServiceKind::Agent,
            ServiceKind::Verifier,
            ServiceKind::Decider,
        ],
        true,
    )
    .await
    .map(|v| v.project)
    .map_err(|e| failure(r.project_error(e)))
}

/// The performer a caller verifies as: a verifier runs runner jobs, everyone else agent jobs.
pub(crate) fn performer(p: &Principal) -> Performer {
    match p {
        Principal::Service(s) if matches!(s.kind, ServiceKind::Verifier | ServiceKind::Decider) => {
            Performer::Runner
        }
        _ => Performer::Agent,
    }
}

/// The replay and idempotency actor of a caller.
pub(crate) fn actor(p: &Principal) -> String {
    match p {
        Principal::Service(s) => format!("service:{}", s.service_account_id),
        Principal::User(u) => format!("user:{}", u.user_id),
    }
}

fn caller(p: &Principal) -> &'static str {
    match p {
        Principal::Service(s) if s.kind == ServiceKind::Verifier => "a verifier service account",
        Principal::Service(s) if s.kind == ServiceKind::Decider => "a decider service account",
        Principal::Service(_) => "an agent service account",
        Principal::User(_) => "a researcher",
    }
}

/// Refuse a caller whose performer differs from the job's, or a verifier the job is not
/// registered to.
pub(crate) fn may_verify(job: &Job, p: &Principal) -> Result<(), Failure> {
    if job.performer != performer(p) {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "{} cannot work on {} verify jobs",
                caller(p),
                job.performer.as_str()
            ),
        ));
    }
    if let Principal::Service(s) = p
        && job.performer == Performer::Runner
        && job.verifier_id.as_deref() != Some(s.name.as_str())
    {
        return Err(domain(
            ErrorCode::Forbidden,
            "this verify job is registered to another verifier",
        ));
    }
    if let Principal::Service(s) = p
        && job.performer == Performer::Runner
        && (s.kind == ServiceKind::Decider) != (job.phase == Phase::Decide)
    {
        return Err(domain(
            ErrorCode::Forbidden,
            "a verifier works on verify jobs and a decider on decide jobs",
        ));
    }
    Ok(())
}

/// Refuse a caller that may not verify this job or does not hold its claim.
pub(crate) fn holder(job: &Job, p: &Principal) -> Result<(), Failure> {
    may_verify(job, p)?;
    if job.claimant() != Some(Claimant::of(p)) {
        return Err(domain(
            ErrorCode::Forbidden,
            "only the verifier that claimed this job can work on it",
        ));
    }
    Ok(())
}
