//! The checks a plan unit passes as the unit document it becomes: science, project fields, track, relations and mentions.
use crate::{
    errors::ApiError,
    requests::RequestContext,
    unit_routes::{Failure, RouteState, domain, internal},
};
use cannery_core::{
    contracts::{
        ContractKind,
        instance::{self, ProjectValidationFailure},
    },
    ids::{ProjectId, UnitId},
    json::{Document, DocumentBuilder, Node, NodeId},
    principal::Principal,
};
use cannery_projects::repo as projects;
use cannery_research::{
    config_repo,
    science::{self, Science, ScienceError},
};
use cannery_units::repo::{self, RelationKind};
use num_bigint::BigInt;
use num_traits::FromPrimitive;
use sqlx::PgConnection;
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
    relations: Vec<(RelationKind, UnitId)>,
    mentions: BTreeSet<UnitId>,
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
            || internal(c, "unit semantic path"),
            |path| violation(&path, message),
        );
    }
    internal(c, "unit science operation")
}
fn subtree(d: &Document, id: NodeId, c: &RequestContext) -> Result<Document, Failure> {
    let mut b = DocumentBuilder::new();
    let id = b.import(d, id).map_err(|_| internal(c, "unit subtree"))?;
    b.finish(id).map_err(|_| internal(c, "unit subtree"))
}
fn text(d: &Document, root: NodeId, key: &str, c: &RequestContext) -> Result<String, Failure> {
    match d.field(root, key).and_then(|id| d.node(id)) {
        Some(Node::String(v)) => Ok(v.clone()),
        _ => Err(internal(c, "unit field invariant")),
    }
}
fn integer(d: &Document, id: NodeId, c: &RequestContext) -> Result<BigInt, Failure> {
    match d.node(id) {
        Some(Node::Integer(v)) => Ok(v.clone()),
        Some(Node::Float(v)) => {
            BigInt::from_f64(*v).ok_or_else(|| internal(c, "unit number conversion"))
        }
        _ => Err(internal(c, "unit number invariant")),
    }
}
/// Check a plan unit entry against the published unit contract, with
/// each path under the request body.
pub(crate) fn unit_contract(
    d: &Document,
    s: &RouteState,
    c: &RequestContext,
) -> Result<(), Failure> {
    let mut pending = vec![(d.root(), 0)];
    while let Some((id, depth)) = pending.pop() {
        if depth >= s.context.validation_walk_budget {
            return Err(internal(c, "unit contract traversal"));
        }
        match d.node(id) {
            Some(Node::Array(values)) => {
                pending.extend(values.iter().rev().map(|id| (*id, depth + 1)));
            }
            Some(Node::Object(values)) => {
                pending.extend(values.iter().rev().map(|(_, id)| (*id, depth + 1)));
            }
            Some(_) => {}
            None => return Err(internal(c, "unit contract node")),
        }
    }
    let violations = s
        .context
        .contracts
        .document_violations(ContractKind::Unit, d)
        .map_err(|_| internal(c, "unit contract"))?;
    if violations.is_empty() {
        return Ok(());
    }
    let details = violations
        .into_iter()
        .map(|v| {
            v.path
                .as_utf8()
                .map(
                    |path| serde_json::json!({"path": format!("body{path}"), "message": v.message}),
                )
                .ok_or_else(|| internal(c, "unit violation path"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(Failure::new(ApiError::from(
        cannery_core::errors::DomainError::new(
            cannery_core::errors::ErrorCode::ValidationFailed,
            "invalid unit",
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
    let Some(schema) = science.unit_fields else {
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
            Err(internal(c, "unit project schema execution"))
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
) -> Result<Option<UnitId>, Failure> {
    Ok(repo::resolve_refs(conn, [(project, n)])
        .await
        .map_err(|_| internal(c, "unit reference"))?
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
            return Err(internal(c, "unit mention recursion"));
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
            None => return Err(internal(c, "unit mention node")),
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
    self_id: Option<UnitId>,
    kept: &BTreeSet<UnitId>,
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
    .map_err(|_| internal(c, "unit science load"))?
    .ok_or_else(|| {
        domain(
            cannery_core::errors::ErrorCode::Conflict,
            "this project has no science revision yet; an admin must create one",
        )
    })?;
    let science = Science::new(config.revision.into(), &config.content, ctx.science)
        .map_err(|e| exceptions(c, e))?;
    science::check_unit(&science, d, ctx.science).map_err(|e| exceptions(c, e))?;
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
    .map_err(|_| internal(c, "unit track load"))?
    .ok_or_else(|| violation("/track", "unknown track"))?;
    if track.state == cannery_tracks::repo::TrackState::Archived {
        return Err(domain(
            cannery_core::errors::ErrorCode::Conflict,
            format!(
                "track '{}' is archived and accepts no new units",
                track
                    .slug
                    .as_utf8()
                    .ok_or_else(|| internal(c, "unit track text"))?
            ),
        ));
    }
    let mut relations = vec![];
    if let Some(Node::Array(v)) = d.field(d.root(), "relations").and_then(|id| d.node(id)) {
        for (index, id) in v.iter().enumerate() {
            let target = d
                .field(*id, "unit")
                .ok_or_else(|| internal(c, "unit relation invariant"))?;
            let (slug, n) = if matches!(d.node(target), Some(Node::Object(_))) {
                (
                    Some(
                        text(d, target, "project", c)?
                            .as_utf8()
                            .ok_or_else(|| internal(c, "unit reference encoding"))?,
                    ),
                    integer(
                        d,
                        d.field(target, "number")
                            .ok_or_else(|| internal(c, "unit reference number"))?,
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
                    &format!("/relations/{index}/unit"),
                    "no such unit, or not readable by you",
                )
            })?;
            let kind = text(d, *id, "kind", c)?
                .as_utf8()
                .ok_or_else(|| internal(c, "unit relation encoding"))?;
            relations.push((
                RelationKind::try_from(kind.as_str())
                    .map_err(|_| internal(c, "unit relation kind"))?,
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
        relations,
        mentions: mentioned,
    })
}
/// A plan unit checked as the unit document it becomes.
pub(crate) struct CheckedUnit {
    pub(crate) science_revision: i32,
    pub(crate) relations: Vec<(RelationKind, UnitId)>,
    pub(crate) mentions: BTreeSet<UnitId>,
}
/// Check the unit document a plan unit becomes: science, project
/// fields, track, relations and mentions.
pub(crate) async fn check_unit(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    document: &Document,
    self_id: Option<UnitId>,
    state: &RouteState,
    c: &RequestContext,
) -> Result<CheckedUnit, Failure> {
    let context = state
        .context
        .mutations
        .as_ref()
        .ok_or_else(|| internal(c, "unit mutation context"))?;
    let kept = match self_id {
        Some(id) => repo::outgoing_relations(conn, id, state.context.repository)
            .await
            .map_err(|_| internal(c, "unit kept relations"))?
            .into_iter()
            .map(|reference| reference.unit_id)
            .collect(),
        None => BTreeSet::new(),
    };
    let prepared = prepare(
        conn, principal, project, document, self_id, &kept, context, c,
    )
    .await?;
    Ok(CheckedUnit {
        science_revision: prepared.revision,
        relations: prepared.relations,
        mentions: prepared.mentions,
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
    self_id: Option<UnitId>,
    budget: usize,
    context: &RequestContext,
) -> Result<BTreeSet<UnitId>, Failure> {
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
#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
