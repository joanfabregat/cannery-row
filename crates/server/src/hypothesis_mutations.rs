//! Draft creation/revision with explicit source validation and serialization profiles.
use crate::{
    authentication::authenticate,
    errors::ApiError,
    hypothesis_routes::{self as routes, Failure, RouteState, body_error, domain, internal},
    requests::RequestContext,
    validation::{self, BodyInput},
};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{
        ContractKind,
        instance::{self, ProjectValidationFailure},
    },
    ids::{HypothesisId, ProjectId},
    json::{Document, DocumentBuilder, Node, NodeId},
    principal::{Principal, Role, ServiceKind},
};
use cannery_hypotheses::repo::{self, HypothesisState, RelationKind};
use cannery_projects::{authz, repo as projects};
use cannery_research::{
    config_repo,
    science::{self, Science, ScienceError},
};
use num_bigint::BigInt;
use num_traits::FromPrimitive;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeSet, sync::Arc};
/// Every profile is supplied by the calling entry point, never inferred globally.
pub struct MutationContext {
    pub science: science::RenderingContext,
    pub config: config_repo::JsonContext,
    pub tracks: cannery_tracks::repo::JsonContext,
    pub mention_walk_budget: usize,
}
struct Prepared {
    revision: i32,
    track: cannery_tracks::repo::Track,
    relations: Vec<(RelationKind, HypothesisId)>,
    mentions: BTreeSet<HypothesisId>,
}
fn violation(path: &str, message: &'static str) -> Failure {
    Failure::new(ApiError::from(
        cannery_core::errors::DomainError::new(
            cannery_core::errors::ErrorCode::ValidationFailed,
            message,
        )
        .with_details(serde_json::json!([{"path":path,"message":message}])),
    ))
}
fn exceptions(c: &RequestContext, error: ScienceError) -> Failure {
    if let ScienceError::Validation { path, message } = error {
        return path.as_utf8().map_or_else(
            || internal(c, "hypothesis semantic path"),
            |path| violation(&path, message),
        );
    }
    internal(c, "hypothesis science operation")
}
fn subtree(d: &Document, id: NodeId, c: &RequestContext) -> Result<Document, Failure> {
    let mut b = DocumentBuilder::new();
    let id = b
        .import(d, id)
        .map_err(|_| internal(c, "hypothesis subtree"))?;
    b.finish(id).map_err(|_| internal(c, "hypothesis subtree"))
}
fn text(d: &Document, root: NodeId, key: &str, c: &RequestContext) -> Result<String, Failure> {
    match d.field(root, key).and_then(|id| d.node(id)) {
        Some(Node::String(v)) => Ok(v.clone()),
        _ => Err(internal(c, "hypothesis field invariant")),
    }
}
fn integer(d: &Document, id: NodeId, c: &RequestContext) -> Result<BigInt, Failure> {
    match d.node(id) {
        Some(Node::Integer(v)) => Ok(v.clone()),
        Some(Node::Float(v)) => {
            BigInt::from_f64(*v).ok_or_else(|| internal(c, "hypothesis number conversion"))
        }
        _ => Err(internal(c, "hypothesis number invariant")),
    }
}
fn fixed_contract(d: &Document, s: &RouteState, c: &RequestContext) -> Result<(), Failure> {
    let mut pending = vec![(d.root(), 0)];
    while let Some((id, depth)) = pending.pop() {
        if depth >= s.context.validation_walk_budget {
            return Err(internal(c, "hypothesis contract traversal"));
        }
        match d.node(id) {
            Some(Node::Array(values)) => {
                pending.extend(values.iter().rev().map(|id| (*id, depth + 1)));
            }
            Some(Node::Object(values)) => {
                pending.extend(values.iter().rev().map(|(_, id)| (*id, depth + 1)));
            }
            Some(_) => {}
            None => return Err(internal(c, "hypothesis contract node")),
        }
    }
    let violations = s
        .context
        .contracts
        .document_violations(ContractKind::Hypothesis, d)
        .map_err(|_| internal(c, "hypothesis contract"))?;
    if violations.is_empty() {
        return Ok(());
    }
    let details = violations
        .into_iter()
        .map(|v| {
            v.path
                .as_utf8()
                .map(|path| serde_json::json!({"path":path,"message":"invalid project field"}))
                .ok_or_else(|| internal(c, "hypothesis violation path"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(Failure::new(ApiError::from(
        cannery_core::errors::DomainError::new(
            cannery_core::errors::ErrorCode::ValidationFailed,
            "invalid hypothesis",
        )
        .with_details(serde_json::json!(details)),
    )))
}
fn fields(
    d: &Document,
    science: &Science<'_>,
    _ctx: &MutationContext,
    c: &RequestContext,
) -> Result<(), Failure> {
    let id = d.field(d.root(), "project_fields");
    let empty = id.is_none_or(|id| {
        matches!(d.node(id), Some(Node::Null))
            || matches!(d.node(id),Some(Node::Object(v)) if v.is_empty())
    });
    let Some(schema) = science.hypothesis_fields else {
        return if empty {
            Ok(())
        } else {
            Err(violation("/project_fields", "must be empty"))
        };
    };
    let schema = Arc::new(subtree(science.content, schema, c)?);
    let value = if let Some(id) = id.filter(|id| !matches!(d.node(*id), Some(Node::Null))) {
        subtree(d, id, c)?
    } else {
        let mut b = DocumentBuilder::new();
        let id = b
            .push(Node::Object(vec![]))
            .map_err(|_| internal(c, "empty fields"))?;
        b.finish(id).map_err(|_| internal(c, "empty fields"))?
    };
    match instance::validate_project_fields(&schema, &value) {
        Ok(()) => Ok(()),
        Err(ProjectValidationFailure::Exception(_)) => {
            Err(internal(c, "hypothesis project schema execution"))
        }
        Err(ProjectValidationFailure::Schema(v)) => {
            let details = v
                .into_iter()
                .map(|v| {
                    v.path
                        .as_utf8()
                        .map(|path| serde_json::json!({"path":path,"message":"invalid project field"}))
                        .ok_or_else(|| internal(c, "project schema path"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Err(Failure::new(ApiError::from(
                cannery_core::errors::DomainError::new(
                    cannery_core::errors::ErrorCode::ValidationFailed,
                    "the project's pinned schema is unusable",
                )
                .with_details(serde_json::json!(details)),
            )))
        }
        Err(ProjectValidationFailure::Document(v)) => {
            let details = v
                .into_iter()
                .map(|violation| {
                    violation
                        .path
                        .as_utf8()
                        .map(|path| {
                            serde_json::json!({
                                "path": format!("/project_fields{path}"),
                                "message": "invalid project field",
                            })
                        })
                        .ok_or_else(|| internal(c, "project field path"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Err(Failure::new(ApiError::from(
                cannery_core::errors::DomainError::new(
                    cannery_core::errors::ErrorCode::ValidationFailed,
                    "invalid project fields",
                )
                .with_details(serde_json::json!(details)),
            )))
        }
    }
}
async fn project_ref(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    slug: Option<&str>,
    c: &RequestContext,
) -> Result<Option<projects::Project>, Failure> {
    if slug.is_none_or(|slug| slug == project.slug) {
        return Ok(Some(project.clone()));
    }
    let Some(other) = projects::get_project_by_slug(conn, slug.unwrap_or_default())
        .await
        .map_err(|e| Failure::new(c.project_error(e)))?
    else {
        return Ok(None);
    };
    let Principal::User(user) = principal else {
        return Ok(None);
    };
    if user.is_admin {
        return Ok(Some(other));
    }
    let role = projects::get_role(conn, other.id, user.user_id)
        .await
        .map_err(|e| Failure::new(c.project_error(e)))?;
    Ok(role.map(|_| other))
}
async fn resolve(
    conn: &mut PgConnection,
    project: ProjectId,
    n: BigInt,
    c: &RequestContext,
) -> Result<Option<HypothesisId>, Failure> {
    Ok(repo::resolve_refs(conn, [(project, n)])
        .await
        .map_err(|_| internal(c, "hypothesis reference"))?
        .into_values()
        .next())
}
fn mentions(
    d: &Document,
    budget: usize,
    c: &RequestContext,
) -> Result<BTreeSet<(Option<String>, BigInt)>, Failure> {
    let mut pending = vec![(d.root(), 0)];
    let mut found = BTreeSet::new();
    while let Some((id, depth)) = pending.pop() {
        if depth >= budget {
            return Err(internal(c, "hypothesis mention recursion"));
        }
        match d.node(id) {
            Some(Node::Array(v)) => pending.extend(v.iter().rev().map(|id| (*id, depth + 1))),
            Some(Node::Object(v)) => pending.extend(v.iter().rev().map(|(_, id)| (*id, depth + 1))),
            Some(Node::String(s)) => {
                let cp = s.codepoints();
                for start in 0..cp.len() {
                    if start > 0
                        && (cannery_core::unicode::word(cp[start - 1])
                            || matches!(cp[start - 1], 35 | 47 | 46 | 45))
                    {
                        continue;
                    }
                    let mut at = start;
                    let slug = if cp[at] == 35 {
                        None
                    } else {
                        if !matches!(cp[at],48..=57|97..=122) {
                            continue;
                        }
                        at += 1;
                        while at < cp.len()
                            && at - start < 63
                            && matches!(cp[at],48..=57|97..=122|45)
                        {
                            at += 1;
                        }
                        if at >= cp.len() || cp[at] != 35 {
                            continue;
                        }
                        Some(
                            cp[start..at]
                                .iter()
                                .filter_map(|v| char::from_u32(*v))
                                .collect::<String>(),
                        )
                    };
                    at += 1;
                    if at >= cp.len() || !matches!(cp[at], 49..=57) {
                        continue;
                    }
                    let digits = at;
                    at += 1;
                    while at < cp.len() && at - digits < 9 && matches!(cp[at], 48..=57) {
                        at += 1;
                    }
                    if at < cp.len() && cannery_core::unicode::word(cp[at]) {
                        continue;
                    }
                    let mut n = BigInt::from(0);
                    for digit in &cp[digits..at] {
                        n = n * 10 + BigInt::from(*digit - 48);
                    }
                    found.insert((slug, n));
                }
            }
            Some(_) => {}
            None => return Err(internal(c, "hypothesis mention node")),
        }
    }
    Ok(found)
}
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Source preparation preserves config, fields, track, relations and mentions order"
)]
async fn prepare(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    d: &Document,
    self_id: Option<HypothesisId>,
    kept: &BTreeSet<HypothesisId>,
    ctx: &MutationContext,
    c: &RequestContext,
) -> Result<Prepared, Failure> {
    let config = config_repo::get_revision(
        conn,
        project.id,
        config_repo::Kind::Science,
        None,
        ctx.config,
    )
    .await
    .map_err(|_| internal(c, "hypothesis science load"))?
    .ok_or_else(|| {
        domain(
            cannery_core::errors::ErrorCode::Conflict,
            "this project has no science revision yet; an admin must create one",
        )
    })?;
    let science = Science::new(config.revision.into(), &config.content, ctx.science)
        .map_err(|e| exceptions(c, e))?;
    science::check_hypothesis(&science, d, ctx.science).map_err(|e| exceptions(c, e))?;
    fields(d, &science, ctx, c)?;
    let track = text(d, d.root(), "track", c)?;
    let track = cannery_tracks::repo::get_track(
        conn,
        project.id,
        &track,
        Some(cannery_tracks::repo::RowLock::Share),
        ctx.tracks,
    )
    .await
    .map_err(|_| internal(c, "hypothesis track load"))?
    .ok_or_else(|| violation("/track", "unknown track"))?;
    if track.state == cannery_tracks::repo::TrackState::Archived {
        return Err(domain(
            cannery_core::errors::ErrorCode::Conflict,
            format!(
                "track '{}' is archived and accepts no drafts",
                track
                    .slug
                    .as_utf8()
                    .ok_or_else(|| internal(c, "hypothesis track text"))?
            ),
        ));
    }
    let mut relations = vec![];
    if let Some(Node::Array(v)) = d.field(d.root(), "relations").and_then(|id| d.node(id)) {
        for (index, id) in v.iter().enumerate() {
            let target = d
                .field(*id, "hypothesis")
                .ok_or_else(|| internal(c, "hypothesis relation invariant"))?;
            let (slug, n) = if matches!(d.node(target), Some(Node::Object(_))) {
                (
                    Some(
                        text(d, target, "project", c)?
                            .as_utf8()
                            .ok_or_else(|| internal(c, "hypothesis reference encoding"))?,
                    ),
                    integer(
                        d,
                        d.field(target, "number")
                            .ok_or_else(|| internal(c, "hypothesis reference number"))?,
                        c,
                    )?,
                )
            } else {
                (None, integer(d, target, c)?)
            };
            let other = if slug.as_ref().is_none_or(|s| s == &project.slug) {
                Some(project.clone())
            } else {
                projects::get_project_by_slug(conn, slug.as_deref().unwrap_or_default())
                    .await
                    .map_err(|e| Failure::new(c.project_error(e)))?
            };
            let target = if let Some(other) = other {
                resolve(conn, other.id, n, c).await?
            } else {
                None
            };
            let target = if let Some(target) = target {
                if !kept.contains(&target)
                    && project_ref(conn, principal, project, slug.as_deref(), c)
                        .await?
                        .is_none()
                {
                    None
                } else {
                    Some(target)
                }
            } else {
                None
            };
            let target = target.filter(|id| Some(*id) != self_id).ok_or_else(|| {
                violation(
                    &format!("/relations/{index}/hypothesis"),
                    "no such hypothesis, or not readable by you",
                )
            })?;
            let kind = text(d, *id, "kind", c)?
                .as_utf8()
                .ok_or_else(|| internal(c, "hypothesis relation encoding"))?;
            relations.push((
                RelationKind::try_from(kind.as_str())
                    .map_err(|_| internal(c, "hypothesis relation kind"))?,
                target,
            ));
        }
    }
    let mentioned = resolve_mentions(
        conn,
        principal,
        project,
        d,
        self_id,
        ctx.mention_walk_budget,
        c,
    )
    .await?;
    Ok(Prepared {
        revision: config.revision,
        track,
        relations,
        mentions: mentioned,
    })
}
/// Resolve readable backlinks using the shared source document scanner.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit source profiles and caller connection"
)]
pub(crate) async fn resolve_mentions(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    document: &Document,
    self_id: Option<HypothesisId>,
    budget: usize,
    context: &RequestContext,
) -> Result<BTreeSet<HypothesisId>, Failure> {
    let mut found = BTreeSet::new();
    for (slug, number) in mentions(document, budget, context)? {
        if let Some(other) = project_ref(conn, principal, project, slug.as_deref(), context).await?
            && let Some(id) = resolve(conn, other.id, number, context).await?
            && Some(id) != self_id
        {
            found.insert(id);
        }
    }
    Ok(found)
}
fn actor(principal: &Principal) -> String {
    match principal {
        Principal::User(v) => format!("user:{}", v.user_id),
        Principal::Service(v) => format!("service:{}", v.service_account_id),
    }
}
fn input(body: &crate::body::DecodedBody) -> BodyInput<'_> {
    match body {
        crate::body::DecodedBody::Missing => BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => BodyInput::RawBytes,
        crate::body::DecodedBody::Json(d) => BodyInput::Json(d),
    }
}
async fn committed(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    detail: crate::hypothesis_wire::Detail,
    state: &RouteState,
    request_context: &RequestContext,
    status: StatusCode,
) -> Result<Response, Failure> {
    transaction
        .commit()
        .await
        .map_err(|_| internal(request_context, "hypothesis mutation commit"))?;
    let bytes = crate::hypothesis_wire::detail(&detail, state.context.response)
        .map_err(|_| internal(request_context, "hypothesis mutation output"))?;
    Ok((status, routes::response(bytes)).into_response())
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep source transaction order and one rollback boundary visible"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses",
    operation_id = "create_draft_api_projects__slug__hypotheses_post",
    summary = "Create Draft",
    params(("slug" = String, Path),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::HypothesisCreateRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::HypothesisOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn create(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &request_context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = routes::path(&mut parts, &state).await?;
    let document = validation::validate_document_body(input(&body))
        .map_err(|e| body_error(&e, &request_context))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&request_context, "hypothesis project path"))?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        Some(Role::Researcher),
        &[ServiceKind::Agent],
        true,
    )
    .await
    .map_err(|e| Failure::new(request_context.project_error(e)))?
    .project;
    fixed_contract(document, &state, &request_context)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::HypothesisCreateRequest>(document)
            .map_err(Failure::new)?;
    let document = &typed_document;
    let key = crate::request_context::first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.chars().count() > 200)
    {
        return Err(domain(
            cannery_core::errors::ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    let hash = crate::hypothesis_mutation_idempotency::hash(
        &project.slug,
        document,
        state.context.request_hash_budget,
    )
    .map_err(|_| internal(&request_context, "hypothesis creation hash"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&request_context, "hypothesis creation transaction"))?;
    let mut status = StatusCode::CREATED;
    let result = async {
        if let Some(key) = &key
            && let Some((prior, id)) = crate::hypothesis_mutation_idempotency::lookup(
                &mut tx,
                &actor(&auth.principal),
                key,
            )
            .await
            .map_err(|_| internal(&request_context, "hypothesis creation replay"))?
        {
            if prior != hash {
                return Err(domain(
                    cannery_core::errors::ErrorCode::Conflict,
                    "this Idempotency-Key was already used with a different request",
                ));
            }
            let id = HypothesisId(
                uuid::Uuid::parse_str(&id)
                    .map_err(|_| internal(&request_context, "hypothesis replay ID"))?,
            );
            let hypothesis = repo::get_hypothesis_by_id(&mut tx, id, state.context.repository)
                .await
                .map_err(|_| internal(&request_context, "hypothesis replay load"))?
                .ok_or_else(|| internal(&request_context, "hypothesis replay invariant"))?;
            status = StatusCode::OK;
            return routes::detail(
                &mut tx,
                &auth.principal,
                &project,
                hypothesis,
                &state.context,
                &request_context,
            )
            .await;
        }
        let context = state
            .context
            .mutations
            .as_ref()
            .ok_or_else(|| internal(&request_context, "hypothesis mutation context"))?;
        let prepared = prepare(
            &mut tx,
            &auth.principal,
            &project,
            document,
            None,
            &BTreeSet::new(),
            context,
            &request_context,
        )
        .await?;
        let number = repo::next_number(&mut tx, project.id)
            .await
            .map_err(|_| internal(&request_context, "hypothesis next number"))?;
        let title = text(document, document.root(), "title", &request_context)?;
        let number_value = BigInt::from(number);
        let creation = repo::CreateHypothesis {
            project_id: project.id,
            number: &number_value,
            track_id: prepared.track.id,
            title: &title,
            principal: &auth.principal,
        };
        let id = repo::create_hypothesis(&mut tx, creation)
            .await
            .map_err(|_| internal(&request_context, "hypothesis create"))?;
        let revision_value = BigInt::from(1);
        let science_revision = BigInt::from(prepared.revision);
        let revision = repo::AddRevision {
            hypothesis_id: id,
            revision: &revision_value,
            content: document,
            science_revision: &science_revision,
            principal: &auth.principal,
        };
        repo::add_revision(&mut tx, revision, state.context.repository)
            .await
            .map_err(|_| internal(&request_context, "hypothesis initial revision"))?;
        repo::replace_relations(&mut tx, id, prepared.relations)
            .await
            .map_err(|_| internal(&request_context, "hypothesis initial relations"))?;
        repo::replace_mentions(
            &mut tx,
            repo::MentionSource::Hypothesis(id),
            prepared.mentions,
        )
        .await
        .map_err(|_| internal(&request_context, "hypothesis initial mentions"))?;
        repo::open_draft_case(
            &mut tx,
            project.id,
            id,
            &revision_value,
            state.context.repository,
        )
        .await
        .map_err(|_| internal(&request_context, "hypothesis initial review case"))?;
        let audit_state = serde_json::json!({
            "number": number,
            "track": prepared.track.slug.as_utf8()
                .ok_or_else(|| internal(&request_context, "track audit encoding"))?,
            "state": "draft",
            "revision": 1,
            "science_revision": prepared.revision,
        });
        let subject = id.to_string();
        let record = Record {
            action: "hypothesis.draft_created",
            subject_type: "hypothesis",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&audit_state),
            reason: None,
            idempotency_key: key.as_deref(),
        };
        audit::record(&mut tx, Attribution::Principal(&auth.principal), record)
            .await
            .map_err(|_| internal(&request_context, "hypothesis creation audit"))?;
        if let Some(key) = &key {
            crate::hypothesis_mutation_idempotency::remember(
                &mut tx,
                &actor(&auth.principal),
                key,
                &hash,
                &subject,
            )
            .await
            .map_err(|_| internal(&request_context, "hypothesis remember"))?;
        }
        let hypothesis = repo::get_hypothesis_by_id(&mut tx, id, state.context.repository)
            .await
            .map_err(|_| internal(&request_context, "hypothesis created load"))?
            .ok_or_else(|| internal(&request_context, "hypothesis created invariant"))?;
        routes::detail(
            &mut tx,
            &auth.principal,
            &project,
            hypothesis,
            &state.context,
            &request_context,
        )
        .await
    }
    .await;
    match result {
        Ok(detail) => committed(tx, detail, &state, &request_context, status).await,
        Err(e) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            Err(e)
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep source transaction order and one rollback boundary visible"
)]
#[utoipa::path(
    put,
    path = "/api/projects/{slug}/hypotheses/{number}",
    operation_id = "revise_draft_api_projects__slug__hypotheses__number__put",
    summary = "Revise Draft",
    params(("slug" = String, Path),
        ("number" = i64, Path)),
    request_body(content = crate::api_models::DraftUpdate, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::HypothesisOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn revise(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &request_context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let body = crate::api_contract::typed_body::<crate::api_models::DraftUpdate>(body)
        .map_err(|error| Failure(Box::new(error.into_response())))?;
    let paths = routes::path(&mut parts, &state).await?;
    let params = validation::hypothesis_parameters(
        paths.get("number").map(String::as_str),
        None,
        &[],
        false,
        false,
    );
    let update = crate::hypothesis_request::update(input(&body));
    let (params, update) = match (params, update) {
        (Ok(preparation), Ok(document)) => (preparation, document),
        (Err(mut preparation), Err(document)) => {
            preparation.append(document);
            return Err(body_error(&preparation, &request_context));
        }
        (Err(e), Ok(_)) | (Ok(_), Err(e)) => return Err(body_error(&e, &request_context)),
    };
    let number = params
        .number
        .ok_or_else(|| internal(&request_context, "hypothesis number"))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&request_context, "hypothesis project path"))?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        Some(Role::Researcher),
        &[ServiceKind::Agent],
        true,
    )
    .await
    .map_err(|e| Failure::new(request_context.project_error(e)))?
    .project;
    let document = &update.document;
    fixed_contract(document, &state, &request_context)?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&request_context, "hypothesis revision transaction"))?;
    let result = async {
        let hypothesis = routes::hypothesis(
            &mut tx,
            project.id,
            &number,
            true,
            &state.context,
            &request_context,
        )
        .await?;
        if let Principal::Service(service) = &auth.principal
            && hypothesis.created_by_service != Some(service.service_account_id)
        {
            return Err(domain(
                cannery_core::errors::ErrorCode::Forbidden,
                "an agent can only revise the drafts it created",
            ));
        }
        if hypothesis.state != HypothesisState::Draft {
            return Err(domain(
                cannery_core::errors::ErrorCode::Conflict,
                format!(
                    "#{number} is {}; only drafts can be revised",
                    hypothesis.state.as_str()
                ),
            ));
        }
        if BigInt::from(hypothesis.revision) != update.revision {
            return Err(domain(
                cannery_core::errors::ErrorCode::StaleRevision,
                format!("#{number} is at revision {}", hypothesis.revision),
            ));
        }
        let kept = repo::outgoing_relations(&mut tx, hypothesis.id, state.context.repository)
            .await
            .map_err(|_| internal(&request_context, "hypothesis kept relations"))?
            .into_iter()
            .map(|reference| reference.hypothesis_id)
            .collect();
        let context = state
            .context
            .mutations
            .as_ref()
            .ok_or_else(|| internal(&request_context, "hypothesis mutation context"))?;
        let prepared = prepare(
            &mut tx,
            &auth.principal,
            &project,
            document,
            Some(hypothesis.id),
            &kept,
            context,
            &request_context,
        )
        .await?;
        let title = text(document, document.root(), "title", &request_context)?;
        let revision = repo::update_draft(&mut tx, hypothesis.id, prepared.track.id, &title)
            .await
            .map_err(|_| internal(&request_context, "hypothesis update"))?;
        let revision_value = BigInt::from(revision);
        let science_revision = BigInt::from(prepared.revision);
        let replacement = repo::AddRevision {
            hypothesis_id: hypothesis.id,
            revision: &revision_value,
            content: document,
            science_revision: &science_revision,
            principal: &auth.principal,
        };
        repo::add_revision(&mut tx, replacement, state.context.repository)
            .await
            .map_err(|_| internal(&request_context, "hypothesis revision insert"))?;
        repo::replace_relations(&mut tx, hypothesis.id, prepared.relations)
            .await
            .map_err(|_| internal(&request_context, "hypothesis replacement relations"))?;
        repo::replace_mentions(
            &mut tx,
            repo::MentionSource::Hypothesis(hypothesis.id),
            prepared.mentions,
        )
        .await
        .map_err(|_| internal(&request_context, "hypothesis replacement mentions"))?;
        repo::open_draft_case(
            &mut tx,
            project.id,
            hypothesis.id,
            &revision_value,
            state.context.repository,
        )
        .await
        .map_err(|_| internal(&request_context, "hypothesis replacement review case"))?;
        let prior = serde_json::json!({
            "revision": hypothesis.revision,
            "track": hypothesis.track_slug.as_utf8()
                .ok_or_else(|| internal(&request_context, "prior track encoding"))?,
        });
        let next = serde_json::json!({
            "revision": revision,
            "track": prepared.track.slug.as_utf8()
                .ok_or_else(|| internal(&request_context, "track audit encoding"))?,
            "science_revision": prepared.revision,
        });
        let subject = hypothesis.id.to_string();
        let record = Record {
            action: "hypothesis.draft_revised",
            subject_type: "hypothesis",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: Some(&prior),
            new_state: Some(&next),
            reason: None,
            idempotency_key: None,
        };
        audit::record(&mut tx, Attribution::Principal(&auth.principal), record)
            .await
            .map_err(|_| internal(&request_context, "hypothesis revision audit"))?;
        let hypothesis =
            repo::get_hypothesis_by_id(&mut tx, hypothesis.id, state.context.repository)
                .await
                .map_err(|_| internal(&request_context, "hypothesis revised load"))?
                .ok_or_else(|| internal(&request_context, "hypothesis revised invariant"))?;
        routes::detail(
            &mut tx,
            &auth.principal,
            &project,
            hypothesis,
            &state.context,
            &request_context,
        )
        .await
    }
    .await;
    match result {
        Ok(detail) => committed(tx, detail, &state, &request_context, StatusCode::OK).await,
        Err(e) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            Err(e)
        }
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
