use crate::{
    Bundle, Entry, EntryKind, Error, Problem, Result, canonical_sha256, semantic, time,
    writer::Writer,
};
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
    ids::{ProjectId, TrackId, UnitId, UserId},
    principal::Role,
};
use cannery_research::{
    config_repo,
    science::{self, RenderingContext},
    steps::{self, Role as StepRole},
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

/// Recommended caller selection; callers still supply it explicitly.
pub const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Five minutes bounds database settlement even for canceled lock waiters.
pub const MAX_STATEMENT_TIMEOUT: Duration = Duration::from_secs(300);
/// Caller-owned schema, JSON and science validation policies.
pub struct ImportContext {
    pub contracts: Arc<ContractValidator>,

    pub rendering: RenderingContext,
    pub json_budget: usize,
    /// Finite per-statement deadline, including advisory-lock waits and canceled work.
    pub statement_timeout: Duration,
}
pub struct ImportOptions<'a> {
    pub slug: &'a str,
    pub science_document: Option<&'a Value>,
    pub dry_run: bool,
    pub allow_missing: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub bundle_sha256: String,
    pub project_created: bool,
    pub science_revision: i32,
    pub science_registered: bool,
    pub unchanged: usize,
    pub policies: usize,
    pub tracks: usize,
    pub units: usize,
    pub attempts: usize,
    pub decisions: usize,
    pub reports: usize,
    pub by_track: BTreeMap<String, usize>,
    pub by_state: BTreeMap<String, usize>,
    pub by_authority: BTreeMap<String, usize>,
    pub gaps: BTreeMap<String, usize>,
    pub missing: Vec<String>,
}
impl Plan {
    #[must_use]
    pub fn nothing_new(&self) -> bool {
        !self.project_created
            && !self.science_registered
            && self.policies + self.tracks + self.units == 0
    }
    #[must_use]
    pub fn describe(&self, dry_run: bool) -> Vec<String> {
        let mut lines = if self.nothing_new() {
            vec![format!(
                "bundle {}: already imported, nothing to do ({} unchanged entries)",
                self.bundle_sha256, self.unchanged
            )]
        } else {
            vec![format!(
                "bundle {}: {}",
                self.bundle_sha256,
                if dry_run { "would import" } else { "imported" }
            )]
        };
        if !self.nothing_new() {
            if self.project_created {
                lines.push("  project: created".into());
            }
            if self.science_registered {
                lines.push(format!(
                    "  science revision {}: registered",
                    self.science_revision
                ));
            }
            lines.push(format!("  {} policies, {} tracks, {} units, {} attempts, {} decisions, {} reports ({} entries unchanged)",self.policies,self.tracks,self.units,self.attempts,self.decisions,self.reports,self.unchanged));
            for (title, counts) in [
                ("units per track", &self.by_track),
                ("units per state", &self.by_state),
                ("measurements per authority", &self.by_authority),
                ("gaps", &self.gaps),
            ] {
                if !counts.is_empty() {
                    lines.push(format!("  {title}:"));
                    lines.extend(
                        counts
                            .iter()
                            .map(|(key, count)| format!("    {key}: {count}")),
                    );
                }
            }
        }
        lines.extend(
            self.missing
                .iter()
                .map(|key| format!("  missing from the bundle, kept as imported: {key}")),
        );
        if dry_run && !self.nothing_new() {
            lines.push("dry run: nothing was written".into());
        }
        lines
    }
}
type Ledger = BTreeMap<(EntryKind, String), Value>;
fn kind(value: &str) -> Result<EntryKind> {
    match value {
        "project" => Ok(EntryKind::Project),
        "track" => Ok(EntryKind::Track),
        "policy" => Ok(EntryKind::Policy),
        "unit" => Ok(EntryKind::Unit),
        _ => Err(Error::CorruptData),
    }
}
fn differences(old: &Value, new: &Value, pointer: &str, found: &mut Vec<String>) {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            let keys = old.keys().chain(new.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                let path = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (old.get(key), new.get(key)) {
                    (Some(old), Some(new)) => differences(old, new, &path, found),
                    _ => found.push(path),
                }
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            for index in 0..old.len().max(new.len()) {
                let path = format!("{pointer}/{index}");
                match (old.get(index), new.get(index)) {
                    (Some(old), Some(new)) => differences(old, new, &path, found),
                    _ => found.push(path),
                }
            }
        }
        (Value::Number(_), Value::Number(_)) if semantic::equal_numbers(old, new) => {}
        _ if canonical_sha256(old) != canonical_sha256(new) => found.push(pointer.into()),
        _ => {}
    }
}
fn compare<'a>(
    bundle: &'a Bundle,
    ledger: &Ledger,
    plan: &mut Plan,
    allow_missing: bool,
) -> Result<Vec<&'a Entry>> {
    let mut present = BTreeSet::new();
    let mut new = Vec::new();
    let mut problems = Vec::new();
    for entry in bundle.entries() {
        let key = (entry.kind, entry.key.clone());
        present.insert(key.clone());
        if let Some(old) = ledger.get(&key) {
            let mut changed = Vec::new();
            differences(old, &entry.content, "", &mut changed);
            if changed.is_empty() {
                plan.unchanged += 1;
            } else {
                problems.extend(changed.into_iter().map(|pointer| {
                    entry.problem(&pointer, "differs from the immutable imported entry")
                }));
            }
        } else {
            new.push(entry);
        }
    }
    plan.missing = ledger
        .keys()
        .filter(|key| !present.contains(*key))
        .map(|(kind, key)| format!("{} {key}", kind.as_str()))
        .collect();
    if !allow_missing {
        problems.extend(plan.missing.iter().map(|key| {
            Problem::new(
                key,
                "",
                "imported previously and missing; allow_missing keeps it",
            )
        }));
    }
    if problems.is_empty() {
        Ok(new)
    } else {
        Err(Error::Refused(problems))
    }
}
#[allow(clippy::too_many_lines)] // Validate and register the pinned science atomically.
async fn science(
    connection: &mut PgConnection,
    project: ProjectId,
    creator: UserId,
    value: Option<&Value>,
    context: &ImportContext,
    plan: &mut Plan,
) -> Result<Value> {
    let profile = config_repo::JsonContext {
        encode_nesting_budget: context.json_budget,
        decode_nesting_budget: context.json_budget,
    };
    let current = config_repo::get_revision(
        connection,
        project,
        config_repo::Kind::Science,
        None,
        profile,
    )
    .await
    .map_err(|_| Error::CorruptData)?;
    if let Some(value) = value {
        let document = semantic::document(value, context.json_budget)?;
        let violations = context
            .contracts
            .document_violations(ContractKind::ScienceRevision, &document)
            .map_err(|_| Error::problem("--science", "", "invalid science schema"))?;
        if !violations.is_empty() {
            return Err(Error::Refused(
                violations
                    .into_iter()
                    .map(|violation| {
                        Problem::new(
                            "--science",
                            &violation.path.as_utf8().unwrap_or_else(|| "/".into()),
                            "does not satisfy the science schema",
                        )
                    })
                    .collect(),
            ));
        }
        let model = science::check_science(&document, context.rendering)
            .map_err(|_| Error::problem("--science", "", "invalid science registrations"))?;
        let scorer = semantic::document(
            value.get("scorer").ok_or(Error::CorruptData)?,
            context.json_budget,
        )?;
        steps::check_step(
            &model,
            &scorer,
            StepRole::Scorer,
            &String::from("/scorer"),
            context.rendering,
        )
        .map_err(|_| Error::problem("--science", "/scorer", "invalid scorer manifest"))?;
        steps::check_validators(&model, context.rendering).map_err(|_| {
            Error::problem(
                "--science",
                "/validators",
                "invalid validator registrations",
            )
        })?;
        if let Some(current) = current {
            let old: Value = serde_json::from_slice(
                &cannery_core::json::encode_http(&current.content, context.json_budget)
                    .map_err(|_| Error::CorruptData)?,
            )
            .map_err(|_| Error::CorruptData)?;
            let mut changed = Vec::new();
            differences(&old, value, "", &mut changed);
            if !changed.is_empty() {
                return Err(Error::problem(
                    "--science",
                    "",
                    "differs from the current revision; register changes through the API",
                ));
            }
            plan.science_revision = current.revision;
            return Ok(old);
        }
        let registered = config_repo::create_revision(
            connection,
            project,
            config_repo::Kind::Science,
            &document,
            None,
            creator,
            profile,
        )
        .await
        .map_err(|_| Error::CorruptData)?;
        plan.science_revision = registered.revision;
        plan.science_registered = true;
        Ok(value.clone())
    } else if let Some(current) = current {
        plan.science_revision = current.revision;
        serde_json::from_slice(
            &cannery_core::json::encode_http(&current.content, context.json_budget)
                .map_err(|_| Error::CorruptData)?,
        )
        .map_err(|_| Error::CorruptData)
    } else {
        Err(Error::problem(
            "--science",
            "",
            "the project has no science revision; give one",
        ))
    }
}
/// Import through one owned transaction. Cancellation returns the connection through
/// `SQLx`'s rollback/reset path; failed validation explicitly rolls back before returning.
/// # Errors
/// Refuses changed history or invalid references; database errors never quote values.
pub async fn run_import(
    pool: &PgPool,
    bundle: &Bundle,
    options: ImportOptions<'_>,
    context: &ImportContext,
) -> Result<Plan> {
    if semantic::text(&bundle.project.content, "slug")? != options.slug {
        return Err(Error::problem(
            &bundle.project.path,
            "/slug",
            "is not the requested project",
        ));
    }
    let mut transaction = pool.begin().await?;
    let result = import_transaction(&mut transaction, bundle, &options, context).await;
    match result {
        Ok(plan) => {
            if options.dry_run || plan.nothing_new() {
                transaction.rollback().await?;
            } else {
                transaction.commit().await?;
            }
            Ok(plan)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}
#[allow(clippy::too_many_lines)] // Ordered validation and writes share one transaction and borrow.
async fn import_transaction(
    connection: &mut PgConnection,
    bundle: &Bundle,
    options: &ImportOptions<'_>,
    context: &ImportContext,
) -> Result<Plan> {
    let millis = context.statement_timeout.as_millis();
    if millis == 0 || millis > MAX_STATEMENT_TIMEOUT.as_millis() {
        return Err(Error::problem(
            "policy",
            "",
            "statement timeout must be between 1 ms and 5 minutes",
        ));
    }
    let timeout = format!("{millis}ms");
    sqlx::query_scalar!("SELECT set_config('statement_timeout',$1,true)", timeout)
        .fetch_one(&mut *connection)
        .await?;
    let mut plan = Plan {
        bundle_sha256: bundle.sha256.clone(),
        ..Plan::default()
    };
    let lock = format!("cannery import {}", options.slug);
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1,0))", lock,)
        .execute(&mut *connection)
        .await?;
    let mut emails = BTreeSet::from([semantic::text(&bundle.project.content, "created_by")?]);
    for entry in &bundle.entries {
        if let Some(decision) = entry.content.get("decision") {
            emails.insert(semantic::text(decision, "decided_by")?);
        }
    }
    let mut users = BTreeMap::new();
    for email in emails {
        let found = cannery_identity::repo::find_users_by_email(connection, email, None, Some(2))
            .await
            .map_err(|_| Error::CorruptData)?;
        if found.len() != 1 {
            return Err(Error::problem(
                "users",
                "",
                "each named email must identify one verified user",
            ));
        }
        users.insert(email.to_owned(), found[0].id);
    }
    let creator = *users
        .get(semantic::text(&bundle.project.content, "created_by")?)
        .ok_or(Error::CorruptData)?;
    let existing = cannery_projects::repo::get_project_by_slug(connection, options.slug)
        .await
        .map_err(|_| Error::CorruptData)?;
    let project = if let Some(project) = existing {
        cannery_projects::repo::lock_project(connection, project.id)
            .await
            .map_err(|_| Error::CorruptData)?;
        project
    } else {
        let project = cannery_projects::repo::create_project(
            connection,
            options.slug,
            semantic::text(&bundle.project.content, "title")?,
            bundle.project.content["description"].as_str().unwrap_or(""),
            creator,
        )
        .await
        .map_err(|_| Error::CorruptData)?
        .ok_or_else(|| Error::problem("--project", "", "created concurrently; retry"))?;
        plan.project_created = true;
        project
    };
    let project_id = project.id;
    let mut researchers = BTreeSet::new();
    for row in sqlx::query!(
        "SELECT user_id AS \"user_id: uuid::Uuid\" FROM memberships WHERE project_id=$1 AND role='researcher'",
        project_id.0 as _,
    )
    .fetch_all(&mut *connection)
    .await?
    {
        researchers.insert(UserId(row.user_id));
    }
    let science = science(
        connection,
        project_id,
        creator,
        options.science_document,
        context,
        &mut plan,
    )
    .await?;
    let mut ledger = Ledger::new();
    for row in sqlx::query!(
        "SELECT kind,key,content::text AS \"content!\" FROM import_entries WHERE project_id=$1",
        project_id.0 as _,
    )
    .fetch_all(&mut *connection)
    .await?
    {
        ledger.insert(
            (kind(&row.kind)?, row.key),
            serde_json::from_str(&row.content).map_err(|_| Error::CorruptData)?,
        );
    }
    let new = compare(bundle, &ledger, &mut plan, options.allow_missing)?;
    let mut tracks = BTreeMap::new();
    for row in sqlx::query!(
        "SELECT slug,id AS \"id: uuid::Uuid\" FROM tracks WHERE project_id=$1",
        project_id.0 as _,
    )
    .fetch_all(&mut *connection)
    .await?
    {
        tracks.insert(row.slug, TrackId(row.id));
    }
    let mut known = BTreeMap::new();
    for row in sqlx::query!(
        "SELECT external_id AS \"external_id!\",id AS \"id: uuid::Uuid\",number FROM units WHERE project_id=$1 AND external_id IS NOT NULL",
        project_id.0 as _,
    ).fetch_all(&mut *connection).await? {known.insert(row.external_id,(UnitId(row.id),row.number));}
    let mut policies = BTreeMap::new();
    for row in sqlx::query!(
        "SELECT id,content::text AS \"content!\" FROM historical_policies WHERE project_id=$1 ORDER BY id,created_at,revision",
        project_id.0 as _,
    ).fetch_all(&mut *connection).await? {policies.insert(row.id,serde_json::from_str(&row.content).map_err(|_|Error::CorruptData)?);}
    let mut track_content = ledger
        .iter()
        .filter(|((kind, _), _)| *kind == EntryKind::Track)
        .map(|((_, key), value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    for entry in &bundle.entries {
        match entry.kind {
            EntryKind::Policy => {
                policies.insert(entry.key.clone(), entry.content.clone());
            }
            EntryKind::Track => {
                track_content.insert(entry.key.clone(), entry.content.clone());
            }
            _ => {}
        }
    }
    let unit_keys = known
        .keys()
        .cloned()
        .chain(
            bundle
                .entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Unit)
                .map(|entry| entry.key.clone()),
        )
        .collect();
    let artifact_uris = bundle
        .entries
        .iter()
        .flat_map(|entry| semantic::items(&entry.content, "attempts"))
        .flat_map(|attempt| semantic::items(attempt, "artifacts"))
        .filter_map(|artifact| artifact["uri"].as_str().map(str::to_owned))
        .collect();
    let knowledge = semantic::Knowledge {
        tracks: &track_content,
        units: &unit_keys,
        policies: &policies,
        artifact_uris: &artifact_uris,
        users: &users,
        researchers: if plan.project_created {
            None
        } else {
            Some(&researchers)
        },
        science: &science,
    };
    let mut problems = Vec::new();
    for entry in &new {
        match entry.kind {
            EntryKind::Track => {
                if tracks.contains_key(&entry.key) {
                    problems.push(entry.problem("/slug", "the project already has this track"));
                }
                if let Some(archived) = entry.content.get("archived_at").and_then(Value::as_str)
                    && time::before(archived, semantic::text(&entry.content, "created_at")?)?
                {
                    problems.push(entry.problem("/archived_at", "before the track was created"));
                }
            }
            EntryKind::Policy => {
                let gates = semantic::items(&entry.content, "gates");
                let keys = gates
                    .iter()
                    .filter_map(|gate| gate["id"].as_str())
                    .collect::<BTreeSet<_>>();
                if keys.len() != gates.len() {
                    problems.push(entry.problem("/gates", "duplicate gate ids"));
                }
            }
            EntryKind::Unit => {
                if known.contains_key(&entry.key) {
                    problems
                        .push(entry.problem("/id", "the project already has this imported unit"));
                }
                problems.extend(semantic::check_unit(entry, &knowledge)?);
            }
            EntryKind::Project => {}
        }
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    if new.is_empty() {
        return Ok(plan);
    }
    let mut writer = Writer {
        connection,
        project: project_id,
        creator,
        users: &users,
        science: &science,
        revision: plan.science_revision,
        bundle,
        reports: &bundle.reports,
        plan: &mut plan,
        tracks,
        policies: &policies,
    };
    if writer.plan.project_created {
        writer
            .audit(
                "import.project_created",
                "project",
                &project_id.to_string(),
                serde_json::json!({"slug":project.slug,"title":project.title}),
            )
            .await?;
        for user in users.values().copied().collect::<BTreeSet<_>>() {
            cannery_projects::repo::set_membership(
                writer.connection,
                project_id,
                user,
                Role::Researcher,
                creator,
            )
            .await
            .map_err(|_| Error::CorruptData)?;
            writer
                .audit(
                    "import.membership",
                    "membership",
                    &format!("{project_id}:{user}"),
                    serde_json::json!({"role":"researcher"}),
                )
                .await?;
        }
    }
    if writer.plan.science_registered {
        writer
            .audit(
                "import.science_registered",
                "config_revision",
                &format!("{project_id}:science:{}", writer.revision),
                serde_json::json!({"revision":writer.revision}),
            )
            .await?;
    }
    for entry in &new {
        match entry.kind {
            EntryKind::Policy => writer.policy(entry).await?,
            EntryKind::Track => writer.track(entry).await?,
            _ => {}
        }
    }
    let mut units = new
        .iter()
        .copied()
        .filter(|entry| entry.kind == EntryKind::Unit)
        .map(|entry| {
            Ok((
                time::moment(semantic::text(&entry.content, "created_at")?)?,
                entry,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    units.sort_by(|(left_time, left), (right_time, right)| {
        left_time.cmp(right_time).then(left.key.cmp(&right.key))
    });
    let mut ids = known
        .iter()
        .map(|(key, (id, _))| (key.clone(), *id))
        .collect();
    let mut numbers = known
        .iter()
        .map(|(key, (_, number))| (key.clone(), *number))
        .collect();
    for (_, entry) in &units {
        writer.unit(entry, &mut ids, &mut numbers).await?;
    }
    for (_, entry) in &units {
        writer.history(entry, &ids, &numbers).await?;
    }
    for entry in new {
        writer.entry(entry).await?;
    }
    writer.audit("import.completed","project",&project_id.to_string(),serde_json::json!({"policies":writer.plan.policies,"tracks":writer.plan.tracks,"units":writer.plan.units,"attempts":writer.plan.attempts,"decisions":writer.plan.decisions,"reports":writer.plan.reports})).await?;
    Ok(plan)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
