//! Native full-text search, ranking, snippets and facets.
use crate::integer::{Integer, argument as integer};
use cannery_core::{
    ids::{ProjectId, ServiceAccountId, UserId},
    timestamps::Timestamp,
};
use chrono::NaiveDateTime;
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MIN_FUZZY_CHARS: usize = 3;
pub const FUZZINESS: &str = "0.5";
pub const FACETS: [&str; 8] = [
    "kind",
    "project",
    "track",
    "unit_state",
    "attempt_state",
    "verdict",
    "decision",
    "actor",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct DocumentId(pub i64);
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct SourceId(pub Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct ActorId(pub Uuid);
macro_rules! domain {
    ($name:ident {$($variant:ident=>$text:literal),+})=>{
        #[derive(Clone,Copy,Debug,Eq,PartialEq)] pub enum $name {$($variant),+}
        impl $name {
            #[must_use] pub const fn as_str(self)->&'static str {match self {$ (Self::$variant=>$text),+}}
            fn parse(value:&str)->Result<Self,SearchError>{match value {$($text=>Ok(Self::$variant)),+,_=>Err(SearchError::CorruptData)}}
        }
    }
}
domain!(Kind {Track=>"track",Unit=>"unit",Attempt=>"attempt",Report=>"report",Verification=>"verification",Writeup=>"writeup",DecisionReason=>"decision_reason",Comment=>"comment"});
domain!(Origin {Live=>"live",Imported=>"imported"});
domain!(Decision {Promote=>"promote",Reject=>"reject",Inconclusive=>"inconclusive",Failed=>"failed",Retry=>"retry",Stop=>"stop"});
domain!(UnitState {Queued=>"queued",Active=>"active",Documenting=>"documenting",Deciding=>"deciding",Promoted=>"promoted",Rejected=>"rejected",Inconclusive=>"inconclusive",Failed=>"failed",Cancelled=>"cancelled"});
domain!(AttemptState {Claimed=>"claimed",Running=>"running",Verifying=>"verifying",Verified=>"verified",Failed=>"failed",Cancelled=>"cancelled",Unreviewed=>"unreviewed"});

#[derive(Clone, Copy)]
pub enum TimeBound {
    Aware(Timestamp),
    Naive(NaiveDateTime),
}
impl std::fmt::Debug for TimeBound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TimeBound([redacted])")
    }
}

/// Filter lists retain duplicates and absent versus empty lists. A missing
/// readable list means installation administrator; an empty one sees nothing.
pub struct Criteria {
    pub readable: Option<Vec<ProjectId>>,
    pub text: Option<String>,
    pub ref_project: Option<String>,
    pub ref_number: Option<BigInt>,
    pub ref_sequence: Option<BigInt>,
    pub projects: Option<Vec<String>>,
    pub kinds: Option<Vec<String>>,
    pub tracks: Option<Vec<String>>,
    pub unit_states: Option<Vec<String>>,
    pub attempt_states: Option<Vec<String>>,
    pub verdicts: Option<Vec<String>>,
    pub decisions: Option<Vec<String>>,
    pub actors: Option<Vec<ActorId>>,
    pub since: Option<TimeBound>,
    pub until: Option<TimeBound>,
}
impl std::fmt::Debug for Criteria {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Criteria([redacted])")
    }
}
impl Criteria {
    #[must_use]
    pub const fn new(readable: Option<Vec<ProjectId>>) -> Self {
        Self {
            readable,
            text: None,
            ref_project: None,
            ref_number: None,
            ref_sequence: None,
            projects: None,
            kinds: None,
            tracks: None,
            unit_states: None,
            attempt_states: None,
            verdicts: None,
            decisions: None,
            actors: None,
            since: None,
            until: None,
        }
    }
    fn parameters(&self) -> Result<Parameters, SearchError> {
        let text = self
            .text
            .as_ref()
            .map(|value| {
                value
                    .as_utf8()
                    .ok_or(SearchError::Database { sqlstate: None })
            })
            .transpose()?;
        if text.as_ref().is_some_and(|value| value.contains('\0'))
            || self
                .ref_project
                .as_ref()
                .is_some_and(|value| value.contains('\0'))
            || [
                &self.projects,
                &self.kinds,
                &self.tracks,
                &self.unit_states,
                &self.attempt_states,
                &self.verdicts,
                &self.decisions,
            ]
            .into_iter()
            .flatten()
            .flatten()
            .any(|value| value.contains('\0'))
        {
            return Err(SearchError::Database { sqlstate: None });
        }
        let (since_aware, since_naive) = bounds(self.since);
        let (until_aware, until_naive) = bounds(self.until);
        Ok(Parameters {
            text,
            fuzzy: self
                .text
                .as_ref()
                .is_some_and(|value| value.codepoints().len() >= MIN_FUZZY_CHARS),
            number: integer(self.ref_number.as_ref())?,
            sequence: integer(self.ref_sequence.as_ref())?,
            since_aware,
            since_naive,
            until_aware,
            until_naive,
        })
    }
}
struct Parameters {
    text: Option<String>,
    fuzzy: bool,
    number: Option<Integer>,
    sequence: Option<Integer>,
    since_aware: Option<Timestamp>,
    since_naive: Option<NaiveDateTime>,
    until_aware: Option<Timestamp>,
    until_naive: Option<NaiveDateTime>,
}
fn bounds(value: Option<TimeBound>) -> (Option<Timestamp>, Option<NaiveDateTime>) {
    match value {
        None => (None, None),
        Some(TimeBound::Aware(value)) => (Some(value), None),
        Some(TimeBound::Naive(value)) => (None, Some(value)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("search row contains an invalid domain value")]
    CorruptData,
}
impl SearchError {
    pub(crate) fn database(error: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|value| {
                    value.len() == 5
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}

pub struct Hit {
    pub id: DocumentId,
    pub kind: Kind,
    pub source_id: SourceId,
    pub project: String,
    pub track: Option<String>,
    pub unit_number: Option<i32>,
    pub unit_title: Option<String>,
    pub attempt_sequence: Option<i32>,
    pub doc_title: String,
    pub snippet: String,
    pub unit_state: Option<UnitState>,
    pub attempt_state: Option<AttemptState>,
    /// Legacy evaluator verdicts are unrestricted stored text.
    pub verdict: Option<String>,
    pub decision: Option<Decision>,
    pub actor_user: Option<UserId>,
    pub actor_service: Option<ServiceAccountId>,
    pub occurred_at: Timestamp,
    pub origin: Origin,
    pub score: f64,
}
impl std::fmt::Debug for Hit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hit([redacted])")
    }
}
#[derive(sqlx::FromRow)]
struct RawHit {
    #[sqlx(rename = "id!: _")]
    id: DocumentId,
    #[sqlx(rename = "kind!")]
    kind: String,
    #[sqlx(rename = "source_id!: _")]
    source_id: SourceId,
    #[sqlx(rename = "project!")]
    project: String,
    #[sqlx(rename = "track?")]
    track: Option<String>,
    #[sqlx(rename = "unit_number?")]
    unit_number: Option<i32>,
    #[sqlx(rename = "unit_title?")]
    unit_title: Option<String>,
    #[sqlx(rename = "attempt_sequence?")]
    attempt_sequence: Option<i32>,
    #[sqlx(rename = "doc_title!")]
    doc_title: String,
    #[sqlx(rename = "snippet!")]
    snippet: String,
    #[sqlx(rename = "unit_state?")]
    unit_state: Option<String>,
    #[sqlx(rename = "attempt_state?")]
    attempt_state: Option<String>,
    #[sqlx(rename = "verdict?")]
    verdict: Option<String>,
    #[sqlx(rename = "decision?")]
    decision: Option<String>,
    #[sqlx(rename = "actor_user: _")]
    actor_user: Option<UserId>,
    #[sqlx(rename = "actor_service: _")]
    actor_service: Option<ServiceAccountId>,
    #[sqlx(rename = "occurred_at!: _")]
    occurred_at: Timestamp,
    #[sqlx(rename = "origin!")]
    origin: String,
    #[sqlx(rename = "score!")]
    score: f64,
}
impl RawHit {
    fn decode(self) -> Result<Hit, SearchError> {
        Ok(Hit {
            id: self.id,
            kind: Kind::parse(&self.kind)?,
            source_id: self.source_id,
            project: self.project,
            track: self.track,
            unit_number: self.unit_number,
            unit_title: self.unit_title,
            attempt_sequence: self.attempt_sequence,
            doc_title: self.doc_title,
            snippet: self.snippet,
            unit_state: self
                .unit_state
                .as_deref()
                .map(UnitState::parse)
                .transpose()?,
            attempt_state: self
                .attempt_state
                .as_deref()
                .map(AttemptState::parse)
                .transpose()?,
            verdict: self.verdict,
            decision: self.decision.as_deref().map(Decision::parse).transpose()?,
            actor_user: self.actor_user,
            actor_service: self.actor_service,
            occurred_at: self.occurred_at,
            origin: Origin::parse(&self.origin)?,
            score: self.score,
        })
    }
}
#[derive(sqlx::FromRow)]
struct RawFacet {
    #[sqlx(rename = "facet!")]
    facet: String,
    value: Option<String>,
    #[sqlx(rename = "count!")]
    count: i64,
}
pub type Facets = BTreeMap<String, BTreeMap<String, i64>>;

/// Set the source transaction-local trigram threshold; transaction ownership is
/// the caller's. Calling in autocommit does not persist the setting.
/// # Errors
/// Returns a sanitized PostgreSQL error.
pub async fn set_fuzziness(conn: &mut PgConnection) -> Result<(), SearchError> {
    sqlx::query_file!("src/sql/set_fuzziness.sql", FUZZINESS)
        .fetch_optional(conn)
        .await
        .map_err(|error| SearchError::database(&error))?;
    Ok(())
}
/// # Errors
/// Reports source argument adaptation, PostgreSQL or stored-domain failure.
pub async fn search(
    conn: &mut PgConnection,
    c: &Criteria,
    after: Option<(f64, &BigInt)>,
    limit: Option<&BigInt>,
) -> Result<Vec<Hit>, SearchError> {
    let p = c.parameters()?;
    let score = after.map(|value| value.0);
    let after_id = integer(after.map(|value| value.1))?;
    let limit = integer(limit)?;
    macro_rules! query {($path:literal,$($facts:expr),*)=>{{let query=sqlx::query_file_as!(RawHit,$path,c.readable.as_deref() as _,p.text.as_deref(),p.fuzzy,c.ref_project.as_deref(),p.number.as_ref() as _,p.sequence.as_ref() as _,c.projects.as_deref(),c.kinds.as_deref(),c.tracks.as_deref(),c.unit_states.as_deref(),c.attempt_states.as_deref(),$( $facts, )*c.actors.as_deref() as _,p.since_aware as _,p.since_naive as _,p.until_aware as _,p.until_naive as _,score,after_id.as_ref() as _,limit.as_ref() as _);query.fetch_all(&mut *conn).await.map_err(|error| SearchError::database(&error))?}}}
    // Python chooses this path by truthiness, not by presence. Empty fact
    // filters are consequently ignored by search but still filter facets.
    let filtered = c.verdicts.as_ref().is_some_and(|values| !values.is_empty())
        || c.decisions
            .as_ref()
            .is_some_and(|values| !values.is_empty());
    let rows = if filtered {
        query!(
            "src/sql/search_filtered.sql",
            c.verdicts.as_deref(),
            c.decisions.as_deref()
        )
    } else {
        query!("src/sql/search_unfiltered.sql",)
    };
    rows.into_iter().map(RawHit::decode).collect()
}
/// # Errors
/// Reports source argument adaptation, PostgreSQL or stored-domain failure.
pub async fn facets(conn: &mut PgConnection, c: &Criteria) -> Result<(i64, Facets), SearchError> {
    let p = c.parameters()?;
    let query = sqlx::query_file_as!(
        RawFacet,
        "src/sql/facets.sql",
        c.readable.as_deref() as _,
        p.text.as_deref(),
        p.fuzzy,
        c.ref_project.as_deref(),
        p.number.as_ref() as _,
        p.sequence.as_ref() as _,
        c.projects.as_deref(),
        c.kinds.as_deref(),
        c.tracks.as_deref(),
        c.unit_states.as_deref(),
        c.attempt_states.as_deref(),
        c.verdicts.as_deref(),
        c.decisions.as_deref(),
        c.actors.as_deref() as _,
        p.since_aware as _,
        p.since_naive as _,
        p.until_aware as _,
        p.until_naive as _
    );
    let rows = query
        .fetch_all(conn)
        .await
        .map_err(|error| SearchError::database(&error))?;
    let mut total = 0;
    let mut found = FACETS
        .into_iter()
        .map(|facet| (facet.to_owned(), BTreeMap::new()))
        .collect::<Facets>();
    for row in rows {
        if row.facet == "total" {
            total = row.count;
        } else {
            found
                .get_mut(&row.facet)
                .ok_or(SearchError::CorruptData)?
                .insert(row.value.ok_or(SearchError::CorruptData)?, row.count);
        }
    }
    Ok((total, found))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
