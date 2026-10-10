//! Application constraints for typed Rust request models.
//! Transport decoding and authentication/dependency ordering belong to adapters.
use cannery_core::{
    errors::{DomainError, ErrorCode},
    ids::{TokenId, UserId},
    json::{Document, Node, NodeId},
    principal::{Role, Scope, ServiceKind},
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::{collections::BTreeSet, fmt};
use uuid::Uuid;

#[derive(Clone)]
pub struct TokenCreate {
    pub name: String,
    pub expires_in_days: BigInt,
    pub scopes: Vec<Scope>,
}
#[derive(Clone)]
pub struct ProjectCreate {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub tracks: Vec<ProjectTrack>,
}
/// A track created with its project: agent mode, the project default producer.
#[derive(Clone)]
pub struct ProjectTrack {
    pub slug: String,
    pub title: String,
    pub description: String,
}
#[derive(Clone)]
pub struct ServiceAccountCreate {
    pub kind: ServiceKind,
    pub name: String,
    pub description: String,
}
#[derive(Clone)]
pub struct MembershipSet {
    pub role: Role,
}
#[derive(Clone)]
pub struct DisableRequest {
    pub reason: String,
}
#[derive(Clone, Copy)]
pub enum BodyModel {
    TokenCreate,
    ProjectCreate,
    ServiceAccountCreate,
    MembershipSet,
    DisableRequest,
}
#[derive(Clone)]
pub enum Body {
    TokenCreate(TokenCreate),
    ProjectCreate(ProjectCreate),
    ServiceAccountCreate(ServiceAccountCreate),
    MembershipSet(MembershipSet),
    DisableRequest(DisableRequest),
}

#[derive(Clone, Eq, PartialEq)]
pub enum Location {
    Field(String),
    Index(usize),
}
impl Location {
    fn field(name: &str) -> Self {
        Self::Field(String::from(name))
    }
}
#[derive(Clone)]
pub struct Problem {
    pub loc: Vec<Location>,
    pub kind: &'static str,
    pub message: &'static str,
}
impl fmt::Debug for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Problem")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
#[derive(Clone)]
pub struct ValidationErrors {
    problems: Vec<Problem>,
}
impl fmt::Debug for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidationErrors")
            .field("count", &self.problems.len())
            .finish_non_exhaustive()
    }
}
impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("request validation failed")
    }
}
impl std::error::Error for ValidationErrors {}
#[derive(Debug, thiserror::Error)]
#[error("validation location cannot be represented as UTF-8")]
pub struct UnrepresentableLocation;
impl ValidationErrors {
    pub(crate) fn from_problems(problems: Vec<Problem>) -> Self {
        Self { problems }
    }
    /// Append another group in the caller's source dependency/signature order.
    pub fn append(&mut self, other: Self) {
        self.problems.extend(other.problems);
    }
    #[must_use]
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }
    /// Source response shape; never includes raw values, contexts or native errors.
    /// # Errors
    /// Lone-surrogate field names cannot be silently replaced with other paths.
    pub fn domain_error(&self) -> Result<DomainError, UnrepresentableLocation> {
        let details = self
            .problems
            .iter()
            .map(|problem| {
                let path = problem
                    .loc
                    .iter()
                    .map(|part| match part {
                        Location::Field(text) => text.as_utf8().ok_or(UnrepresentableLocation),
                        Location::Index(index) => Ok(index.to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                Ok(serde_json::json!({"path":path,"message":problem.message}))
            })
            .collect::<Result<Vec<_>, UnrepresentableLocation>>()?;
        Ok(
            DomainError::new(ErrorCode::ValidationFailed, "request validation failed")
                .with_details(serde_json::Value::Array(details)),
        )
    }
    fn one(loc: Vec<Location>, issue: Issue) -> Self {
        Self {
            problems: vec![Problem {
                loc,
                kind: issue.kind,
                message: issue.message,
            }],
        }
    }
}
#[derive(Clone, Copy)]
struct Issue {
    kind: &'static str,
    message: &'static str,
}
const fn issue(kind: &'static str, message: &'static str) -> Issue {
    Issue { kind, message }
}
const MISSING: Issue = issue("missing", "Field required");
const STRING_TYPE: Issue = issue("string_type", "Input should be a valid string");
const STRING_UNICODE: Issue = issue(
    "string_unicode",
    "Input should be a valid string, unable to parse raw data as a unicode string",
);
type Checked<T> = Result<T, ValidationErrors>;

struct Fields<'a> {
    document: &'a Document,
    root: NodeId,
    problems: Vec<Problem>,
}

pub(crate) fn validate_upload(
    input: BodyInput<'_>,
) -> Checked<crate::upload_request::UploadRequest> {
    let mut fields = Fields::from_input(input)?;
    let role = fields.field("role", None, |node| upload_text(node, "role"));
    let name = fields.field("name", None, |node| upload_text(node, "name"));
    let size_bytes = fields.field("size_bytes", None, |node| {
        let value = integer(node)?;
        if value < BigInt::from(0) {
            Err(issue(
                "greater_than_equal",
                "Input should be greater than or equal to 0",
            ))
        } else {
            Ok(value)
        }
    });
    let sha256 = fields.field("sha256", None, |node| upload_text(node, "sha256"));
    let media_type = fields.field("media_type", None, |node| upload_text(node, "media_type"));
    fields.extras(&["role", "name", "size_bytes", "sha256", "media_type"]);
    match (role, name, size_bytes, sha256, media_type) {
        (Some(role), Some(name), Some(size_bytes), Some(sha256), Some(media_type))
            if fields.problems.is_empty() =>
        {
            Ok(crate::upload_request::UploadRequest {
                role,
                name,
                size_bytes,
                sha256,
                media_type,
            })
        }
        _ => Err(fields.failure()),
    }
}
fn upload_text(node: &Node, field: &str) -> Result<String, Issue> {
    let value = utf8_text(node, TextRule::Plain)?;
    if field == "media_type" && value.chars().count() > 200 {
        return Err(issue(
            "string_too_long",
            "String should have at most 200 characters",
        ));
    }
    // The four source patterns contain only ASCII classes and full anchors.
    let valid = match field {
        "role" => {
            (1..=64).contains(&value.len())
                && value.as_bytes()[0].is_ascii_lowercase()
                && value
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        }
        "name" => {
            (1..=128).contains(&value.len()) && value.as_bytes()[0].is_ascii_alphanumeric()
                || (1..=128).contains(&value.len()) && matches!(value.as_bytes()[0], b'_' | b'-')
        }
        "sha256" => {
            value.len() == 64
                && value
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        }
        "media_type" => value.split_once('/').is_some_and(|(a, b)| {
            !a.is_empty()
                && a.bytes().all(|c| c.is_ascii_lowercase())
                && !b.is_empty()
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'_' | b'-'))
        }),
        _ => false,
    } && (field != "name"
        || value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')));
    if valid {
        Ok(value)
    } else {
        Err(issue(
            "string_pattern_mismatch",
            "String should match pattern",
        ))
    }
}
pub(crate) fn validate_upload_presign(input: BodyInput<'_>) -> Checked<Option<Vec<BigInt>>> {
    if matches!(input, BodyInput::Missing)
        || matches!(input, BodyInput::Json(d) if matches!(d.node(d.root()), Some(Node::Null)))
    {
        return Ok(None);
    }
    let mut fields = Fields::from_input(input)?;
    let id = fields.document.field(fields.root, "part_numbers");
    let mut numbers = None;
    if let Some(id) = id {
        match fields.document.node(id) {
            Some(Node::Null) => {}
            Some(Node::Array(items)) if items.len() > 100 => fields.push(
                vec![Location::field("body"), Location::field("part_numbers")],
                issue(
                    "too_long",
                    "List should have at most 100 items after validation",
                ),
            ),
            Some(Node::Array(items)) => {
                let mut values = Vec::new();
                for (index, id) in items.iter().enumerate() {
                    match fields.document.node(*id).map(integer) {
                        Some(Ok(value)) => values.push(value),
                        Some(Err(error)) => fields.push(
                            vec![
                                Location::field("body"),
                                Location::field("part_numbers"),
                                Location::Index(index),
                            ],
                            error,
                        ),
                        None => unreachable!("lossless document nodes are checked"),
                    }
                }
                if fields.problems.is_empty() && !(1..=100).contains(&values.len()) {
                    fields.push(
                        vec![Location::field("body"), Location::field("part_numbers")],
                        if values.is_empty() {
                            issue(
                                "too_short",
                                "List should have at least 1 item after validation",
                            )
                        } else {
                            issue(
                                "too_long",
                                "List should have at most 100 items after validation",
                            )
                        },
                    );
                }
                numbers = Some(values);
            }
            _ => fields.push(
                vec![Location::field("body"), Location::field("part_numbers")],
                issue("list_type", "Input should be a valid list"),
            ),
        }
    }
    fields.extras(&["part_numbers"]);
    if fields.problems.is_empty() {
        Ok(numbers)
    } else {
        Err(fields.failure())
    }
}
impl<'a> Fields<'a> {
    fn from_input(input: BodyInput<'a>) -> Checked<Self> {
        match input {
            BodyInput::Json(document) if matches!(document.node(document.root()), Some(Node::Object(fields)) if fields.iter().any(|(name, _)| name.as_utf8().is_none())) =>
            {
                // Pydantic extracts model keys before validating declared fields.
                Err(ValidationErrors::one(
                    vec![Location::field("body")],
                    STRING_UNICODE,
                ))
            }
            BodyInput::Json(document) => Self::new(document),
            BodyInput::Missing => Err(ValidationErrors::one(
                vec![Location::field("body")],
                MISSING,
            )),
            BodyInput::RawBytes => Err(ValidationErrors::one(
                vec![Location::field("body")],
                issue(
                    "model_attributes_type",
                    "Input should be a valid dictionary or object to extract fields from",
                ),
            )),
        }
    }
    fn new(document: &'a Document) -> Checked<Self> {
        let root = document.root();
        match document.node(root) {
            Some(Node::Object(_)) => Ok(Self {
                document,
                root,
                problems: Vec::new(),
            }),
            Some(Node::Null) => Err(ValidationErrors::one(
                vec![Location::field("body")],
                MISSING,
            )),
            _ => Err(ValidationErrors::one(
                vec![Location::field("body")],
                issue(
                    "model_attributes_type",
                    "Input should be a valid dictionary or object to extract fields from",
                ),
            )),
        }
    }
    fn field<T>(
        &mut self,
        name: &str,
        default: Option<T>,
        validate: impl FnOnce(&Node) -> Result<T, Issue>,
    ) -> Option<T> {
        let loc = vec![Location::field("body"), Location::field(name)];
        let Some(node) = self
            .document
            .field(self.root, name)
            .and_then(|id| self.document.node(id))
        else {
            if default.is_none() {
                self.push(loc, MISSING);
            }
            return default;
        };
        match validate(node) {
            Ok(value) => Some(value),
            Err(error) => {
                self.push(loc, error);
                None
            }
        }
    }
    fn push(&mut self, loc: Vec<Location>, error: Issue) {
        self.problems.push(Problem {
            loc,
            kind: error.kind,
            message: error.message,
        });
    }
    fn extras(&mut self, allowed: &[&str]) {
        if let Some(Node::Object(fields)) = self.document.node(self.root) {
            for (name, _) in fields {
                if !allowed.iter().any(|allowed| name.equals_utf8(allowed)) {
                    self.problems.push(Problem {
                        loc: vec![Location::field("body"), Location::Field(name.clone())],
                        kind: "extra_forbidden",
                        message: "Extra inputs are not permitted",
                    });
                }
            }
        }
    }
    fn failure(self) -> ValidationErrors {
        ValidationErrors {
            problems: self.problems,
        }
    }
}

#[derive(Clone, Copy)]
enum TextRule {
    Plain,
    Bounded(usize, usize),
    Trimmed(usize),
    TokenName,
    Slug,
    Email,
}
fn text(node: &Node, rule: TextRule) -> Result<String, Issue> {
    let Node::String(value) = node else {
        return Err(STRING_TYPE);
    };
    if matches!(rule, TextRule::Plain) {
        return Ok(value.clone());
    }
    let utf8 = value.as_utf8().ok_or(STRING_UNICODE)?;
    let value = if matches!(rule, TextRule::Trimmed(_) | TextRule::TokenName) {
        utf8.trim_matches(white_space)
    } else {
        &utf8
    };
    let length = value.chars().count();
    let bounds = match rule {
        TextRule::Bounded(minimum, maximum) => Some((minimum, maximum)),
        TextRule::Trimmed(maximum) => Some((1, maximum)),
        TextRule::TokenName => Some((1, 100)),
        TextRule::Email => Some((3, usize::MAX)),
        _ => None,
    };
    if let Some((minimum, maximum)) = bounds {
        if length < minimum {
            return Err(issue(
                "string_too_short",
                if minimum == 3 {
                    "String should have at least 3 characters"
                } else {
                    "String should have at least 1 character"
                },
            ));
        }
        if length > maximum {
            return Err(issue(
                "string_too_long",
                if maximum == 100 {
                    "String should have at most 100 characters"
                } else if maximum == 128 {
                    "String should have at most 128 characters"
                } else {
                    "String should have at most 200 characters"
                },
            ));
        }
    }
    if matches!(rule, TextRule::Slug)
        && !((1..=63).contains(&length)
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || (index > 0 && byte == b'-')
            }))
    {
        return Err(issue(
            "string_pattern_mismatch",
            "String should match pattern '^[a-z0-9][a-z0-9-]{0,62}$'",
        ));
    }
    if matches!(rule, TextRule::TokenName)
        && (value.contains(';') || !value.chars().all(|value| printable(u32::from(value))))
    {
        return Err(issue(
            "value_error",
            "Value error, a token name has printable characters only, and no ';'",
        ));
    }
    Ok(String::from(value))
}
fn utf8_text(node: &Node, rule: TextRule) -> Result<String, Issue> {
    text(node, rule)?.as_utf8().ok_or(STRING_UNICODE)
}
// Unicode White_Space15.1; differs from Python str.strip's additional C0 separators.
fn white_space(value: char) -> bool {
    value.is_whitespace()
}
/// Native Unicode control-character classification for diagnostic text.
#[must_use]
pub fn printable(point: u32) -> bool {
    cannery_core::text::printable(point)
}
fn integer(node: &Node) -> Result<BigInt, Issue> {
    let value = match node {
        Node::Integer(value) => value.to_i64(),
        _ => None,
    };
    value
        .map(BigInt::from)
        .ok_or_else(|| issue("int_parsing", "Input should be a signed 64-bit integer"))
}
fn query_integer(node: &Node) -> Result<BigInt, Issue> {
    match node {
        Node::String(value) => value
            .parse::<i64>()
            .map(BigInt::from)
            .map_err(|_| issue("int_parsing", "Input should be a signed 64-bit integer")),
        _ => integer(node),
    }
}
fn positive_integer(node: &Node) -> Result<BigInt, Issue> {
    let value = integer(node)?;
    if value <= BigInt::from(0) {
        Err(issue("greater_than", "Input should be greater than 0"))
    } else {
        Ok(value)
    }
}
fn positive_i32_integer(node: &Node) -> Result<BigInt, Issue> {
    let value = integer(node)?;
    if value < BigInt::from(1) {
        return Err(issue(
            "greater_than_equal",
            "Input should be greater than or equal to 1",
        ));
    }
    if value > BigInt::from(i32::MAX) {
        return Err(issue(
            "less_than_equal",
            "Input should be less than or equal to 2147483647",
        ));
    }
    Ok(value)
}
fn limit(node: &Node) -> Result<usize, Issue> {
    let value = query_integer(node)?;
    if value < BigInt::from(1) {
        return Err(issue(
            "greater_than_equal",
            "Input should be greater than or equal to 1",
        ));
    }
    if value > BigInt::from(200) {
        return Err(issue(
            "less_than_equal",
            "Input should be less than or equal to 200",
        ));
    }
    value
        .to_usize()
        .ok_or_else(|| issue("int_type", "Input should be a valid integer"))
}
fn scope(node: &Node) -> Result<Scope, Issue> {
    match node {
        Node::String(value) if value.equals_utf8("read") => Ok(Scope::Read),
        Node::String(value) if value.equals_utf8("write") => Ok(Scope::Write),
        _ => Err(issue("literal_error", "Input should be 'read' or 'write'")),
    }
}
fn service_kind(node: &Node) -> Result<ServiceKind, Issue> {
    match node {
        Node::String(value) if value.equals_utf8("agent") => Ok(ServiceKind::Agent),
        Node::String(value) if value.equals_utf8("experimenter") => Ok(ServiceKind::Experimenter),
        Node::String(value) if value.equals_utf8("verifier") => Ok(ServiceKind::Verifier),
        Node::String(value) if value.equals_utf8("decider") => Ok(ServiceKind::Decider),
        _ => Err(issue(
            "literal_error",
            "Input should be 'agent', 'experimenter', 'verifier' or 'decider'",
        )),
    }
}
fn role(node: &Node) -> Result<Role, Issue> {
    match node {
        Node::String(value) if value.equals_utf8("viewer") => Ok(Role::Viewer),
        Node::String(value) if value.equals_utf8("member") => Ok(Role::Member),
        Node::String(value) if value.equals_utf8("researcher") => Ok(Role::Researcher),
        _ => Err(issue(
            "literal_error",
            "Input should be 'viewer', 'member' or 'researcher'",
        )),
    }
}
/// Validate the token model, preserving scope duplicates until its business check.
/// # Errors
/// Returns ordered field/index errors followed by extras in insertion order.
pub fn token_create(document: &Document) -> Checked<TokenCreate> {
    let mut fields = Fields::new(document)?;
    let name = fields.field("name", None, |node| utf8_text(node, TextRule::TokenName));
    let expires_in_days = fields.field("expires_in_days", None, positive_integer);
    let scopes = token_scopes(&mut fields);
    fields.extras(&["name", "expires_in_days", "scopes"]);
    match (name, expires_in_days, scopes) {
        (Some(name), Some(expires_in_days), Some(scopes)) if fields.problems.is_empty() => {
            Ok(TokenCreate {
                name,
                expires_in_days,
                scopes,
            })
        }
        _ => Err(fields.failure()),
    }
}
fn token_scopes(fields: &mut Fields<'_>) -> Option<Vec<Scope>> {
    let loc = vec![Location::field("body"), Location::field("scopes")];
    let node = fields
        .document
        .field(fields.root, "scopes")
        .and_then(|id| fields.document.node(id));
    let Some(node) = node else {
        fields.push(loc, MISSING);
        return None;
    };
    let Node::Array(values) = node else {
        fields.push(loc, issue("list_type", "Input should be a valid list"));
        return None;
    };
    if values.is_empty() {
        fields.push(
            loc,
            issue(
                "too_short",
                "List should have at least 1 item after validation, not 0",
            ),
        );
        return None;
    }
    let mut scopes = Vec::with_capacity(values.len());
    for (index, id) in values.iter().enumerate() {
        let parsed = fields
            .document
            .node(*id)
            .ok_or(issue("literal_error", "Input should be 'read' or 'write'"))
            .and_then(scope);
        match parsed {
            Ok(scope) => scopes.push(scope),
            Err(error) => {
                let mut indexed = loc.clone();
                indexed.push(Location::Index(index));
                fields.push(indexed, error);
            }
        }
    }
    Some(scopes)
}
/// Validate project creation, retaining unconstrained Python descriptions.
/// # Errors
/// Returns source-compatible ordered validation locations.
pub fn project_create(document: &Document) -> Checked<ProjectCreate> {
    let mut fields = Fields::new(document)?;
    let slug = fields.field("slug", None, |node| utf8_text(node, TextRule::Slug));
    let title = fields.field("title", None, |node| {
        utf8_text(node, TextRule::Trimmed(200))
    });
    let description = fields.field("description", Some(String::new()), |node| {
        text(node, TextRule::Plain)
    });
    let tracks = project_tracks(&mut fields);
    fields.extras(&["slug", "title", "description", "tracks"]);
    match (slug, title, description, tracks) {
        (Some(slug), Some(title), Some(description), Some(tracks))
            if fields.problems.is_empty() =>
        {
            Ok(ProjectCreate {
                slug,
                title,
                description,
                tracks,
            })
        }
        _ => Err(fields.failure()),
    }
}
/// The most tracks a project starts with.
pub const PROJECT_TRACKS_MAX: usize = 32;
fn project_tracks(fields: &mut Fields<'_>) -> Option<Vec<ProjectTrack>> {
    let loc = vec![Location::field("body"), Location::field("tracks")];
    let document = fields.document;
    let Some(node) = document
        .field(fields.root, "tracks")
        .and_then(|id| document.node(id))
    else {
        fields.push(loc, MISSING);
        return None;
    };
    let Node::Array(items) = node else {
        fields.push(loc, issue("list_type", "Input should be a valid list"));
        return None;
    };
    if items.is_empty() {
        fields.push(
            loc,
            issue(
                "too_short",
                "List should have at least 1 item after validation, not 0",
            ),
        );
        return None;
    }
    if items.len() > PROJECT_TRACKS_MAX {
        fields.push(
            loc,
            issue(
                "too_long",
                "List should have at most 32 items after validation",
            ),
        );
        return None;
    }
    let mut tracks = Vec::with_capacity(items.len());
    let mut slugs = BTreeSet::new();
    for (index, id) in items.iter().enumerate() {
        let mut at = loc.clone();
        at.push(Location::Index(index));
        if let Some(track) = project_track(fields, *id, &at) {
            if !slugs.insert(track.slug.clone()) {
                let mut at = at.clone();
                at.push(Location::field("slug"));
                fields.push(
                    at,
                    issue("value_error", "Value error, track slugs must be unique"),
                );
            }
            tracks.push(track);
        }
    }
    Some(tracks)
}
fn project_track(fields: &mut Fields<'_>, id: NodeId, loc: &[Location]) -> Option<ProjectTrack> {
    let document = fields.document;
    let at = |name: &str| {
        let mut loc = loc.to_vec();
        loc.push(Location::field(name));
        loc
    };
    let Some(Node::Object(entries)) = document.node(id) else {
        fields.push(
            loc.to_vec(),
            issue(
                "model_type",
                "Input should be a valid dictionary or instance of ProjectTrack",
            ),
        );
        return None;
    };
    let mut value =
        |name: &str, required: bool, rule: fn(&Node) -> Result<String, Issue>| match document
            .field(id, name)
            .and_then(|id| document.node(id))
        {
            Some(node) => match rule(node) {
                Ok(value) => Some(value),
                Err(error) => {
                    fields.push(at(name), error);
                    None
                }
            },
            None if required => {
                fields.push(at(name), MISSING);
                None
            }
            None => Some(String::new()),
        };
    let slug = value("slug", true, |node| utf8_text(node, TextRule::Slug));
    let title = value("title", true, track_text);
    let description = value("description", false, |node| text(node, TextRule::Plain));
    for (name, _) in entries {
        if !["slug", "title", "description"]
            .iter()
            .any(|allowed| name.equals_utf8(allowed))
        {
            let mut loc = loc.to_vec();
            loc.push(Location::Field(name.clone()));
            fields.push(
                loc,
                issue("extra_forbidden", "Extra inputs are not permitted"),
            );
        }
    }
    Some(ProjectTrack {
        slug: slug?,
        title: title?,
        description: description?,
    })
}
/// A track title: required free text, not whitespace-only.
fn track_text(node: &Node) -> Result<String, Issue> {
    let value = utf8_text(node, TextRule::Plain)?;
    if value.trim().is_empty() {
        return Err(issue(
            "string_pattern_mismatch",
            "String should match pattern '\\S'",
        ));
    }
    Ok(value)
}
/// Validate service creation without application/database side effects.
/// # Errors
/// Returns source-compatible ordered validation locations.
pub fn service_account_create(document: &Document) -> Checked<ServiceAccountCreate> {
    let mut fields = Fields::new(document)?;
    let kind = fields.field("kind", None, service_kind);
    let name = fields.field("name", None, |node| utf8_text(node, TextRule::Slug));
    let description = fields.field("description", Some(String::new()), |node| {
        text(node, TextRule::Plain)
    });
    fields.extras(&["kind", "name", "description"]);
    match (kind, name, description) {
        (Some(kind), Some(name), Some(description)) if fields.problems.is_empty() => {
            Ok(ServiceAccountCreate {
                kind,
                name,
                description,
            })
        }
        _ => Err(fields.failure()),
    }
}
/// Validate a membership's closed role enum.
/// # Errors
/// Returns source-compatible ordered validation locations.
pub fn membership_set(document: &Document) -> Checked<MembershipSet> {
    let mut fields = Fields::new(document)?;
    let role = fields.field("role", None, role);
    fields.extras(&["role"]);
    match role {
        Some(role) if fields.problems.is_empty() => Ok(MembershipSet { role }),
        _ => Err(fields.failure()),
    }
}
/// Validate a trimmed, nonempty disable reason.
/// # Errors
/// Returns source-compatible ordered validation locations.
pub fn disable_request(document: &Document) -> Checked<DisableRequest> {
    let mut fields = Fields::new(document)?;
    let reason = fields.field("reason", None, |node| {
        utf8_text(node, TextRule::Trimmed(200))
    });
    fields.extras(&["reason"]);
    match reason {
        Some(reason) if fields.problems.is_empty() => Ok(DisableRequest { reason }),
        _ => Err(fields.failure()),
    }
}
/// Select a known body model. The HTTP adapter chooses the model from its route.
/// # Errors
/// Returns ordered validation problems without offending input values.
pub fn validate_body(document: &Document, model: BodyModel) -> Checked<Body> {
    match model {
        BodyModel::TokenCreate => token_create(document).map(Body::TokenCreate),
        BodyModel::ProjectCreate => project_create(document).map(Body::ProjectCreate),
        BodyModel::ServiceAccountCreate => {
            service_account_create(document).map(Body::ServiceAccountCreate)
        }
        BodyModel::MembershipSet => membership_set(document).map(Body::MembershipSet),
        BodyModel::DisableRequest => disable_request(document).map(Body::DisableRequest),
    }
}

/// Transport distinctions required by `FastAPI`'s model-body validation.
#[derive(Clone, Copy)]
pub enum BodyInput<'a> {
    Missing,
    Json(&'a Document),
    /// A non-JSON content type yields bytes, which fail model extraction.
    RawBytes,
}

/// Validate a required model body after transport decoding and authentication.
/// # Errors
/// Missing bodies produce `missing`; non-JSON bytes produce `model_attributes_type`.
pub fn validate_body_input(input: BodyInput<'_>, model: BodyModel) -> Checked<Body> {
    match input {
        BodyInput::Json(document) => validate_body(document, model),
        BodyInput::Missing => Err(ValidationErrors::one(
            vec![Location::field("body")],
            MISSING,
        )),
        BodyInput::RawBytes => Err(ValidationErrors::one(
            vec![Location::field("body")],
            issue(
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from",
            ),
        )),
    }
}
/// Apply configured lifetime bound and source sorted/deduplicated scopes.
/// # Errors
/// A model-valid lifetime above the setting returns `validation_failed` with null details.
pub fn check_token_request(
    body: &TokenCreate,
    max_days: &BigInt,
) -> Result<Vec<Scope>, DomainError> {
    if &body.expires_in_days > max_days {
        return Err(DomainError::new(
            ErrorCode::ValidationFailed,
            format!("expires_in_days must be at most {max_days}"),
        ));
    }
    Ok(body
        .scopes
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

#[derive(Clone, Copy)]
pub enum Parameter {
    TokenId,
    UserId,
    BeforeUuid,
    Limit,
    Email,
    BeforeString,
    ReturnTo,
    Slug,
    ConfigKind,
    ConfigRevision,
    ConfigBefore,
    TrackState,
    HistoryBefore,
}
pub enum ParameterValue {
    TokenId(TokenId),
    UserId(UserId),
    BeforeUuid(Option<Uuid>),
    Limit(usize),
    Email(String),
    BeforeString(Option<String>),
    ReturnTo(Option<String>),
    Slug(String),
    ConfigKind(&'static str),
    ConfigRevision(BigInt),
    ConfigBefore(Option<BigInt>),
    TrackState(Option<cannery_tracks::repo::TrackState>),
    HistoryBefore(Option<BigInt>),
}
fn uuid(node: &Node) -> Result<Uuid, Issue> {
    let Node::String(value) = node else {
        return Err(issue(
            "uuid_type",
            "UUID input should be a string, bytes or UUID object",
        ));
    };
    let value = value.as_utf8().ok_or(STRING_UNICODE)?;
    Uuid::parse_str(&value).map_err(|_| issue("uuid_parsing", "Input should be a valid UUID"))
}
impl Parameter {
    fn location(self) -> Vec<Location> {
        let (location, name) = match self {
            Self::TokenId => ("path", "token_id"),
            Self::UserId => ("path", "user_id"),
            Self::BeforeUuid | Self::BeforeString | Self::ConfigBefore | Self::HistoryBefore => {
                ("query", "before")
            }
            Self::TrackState => ("query", "state"),
            Self::Limit => ("query", "limit"),
            Self::Email => ("query", "email"),
            Self::ReturnTo => ("query", "return_to"),
            Self::Slug => ("path", "slug"),
            Self::ConfigKind => ("path", "kind"),
            Self::ConfigRevision => ("path", "revision"),
        };
        vec![Location::field(location), Location::field(name)]
    }
}
/// Validate an already-decoded parameter; omitted defaults belong to the adapter.
/// # Errors
/// Returns a problem at the actual route parameter's path/query location.
pub fn validate_parameter(document: &Document, parameter: Parameter) -> Checked<ParameterValue> {
    match document.node(document.root()) {
        Some(node) => validate_parameter_node(node, parameter),
        None => Err(ValidationErrors::one(parameter.location(), STRING_TYPE)),
    }
}
/// Validate a decoded scalar path/query value without constructing a document.
/// # Errors
/// Returns ordered source-shaped parameter errors.
pub fn validate_parameter_node(node: &Node, parameter: Parameter) -> Checked<ParameterValue> {
    let result = (|| match parameter {
        Parameter::TrackState => match node {
            Node::Null => Ok(ParameterValue::TrackState(None)),
            Node::String(value) => {
                let value = value.as_utf8().ok_or(STRING_UNICODE)?;
                cannery_tracks::repo::TrackState::try_from(value.as_str())
                    .map(|value| ParameterValue::TrackState(Some(value)))
                    .map_err(|_| {
                        issue(
                            "literal_error",
                            "Input should be 'planning', 'active', 'paused' or 'archived'",
                        )
                    })
            }
            _ => Err(issue(
                "literal_error",
                "Input should be 'planning', 'active', 'paused' or 'archived'",
            )),
        },
        Parameter::HistoryBefore => {
            if matches!(node, Node::Null) {
                return Ok(ParameterValue::HistoryBefore(None));
            }
            let value = query_integer(node)?;
            if value < BigInt::from(0) {
                return Err(issue(
                    "greater_than_equal",
                    "Input should be greater than or equal to 0",
                ));
            }
            Ok(ParameterValue::HistoryBefore(Some(value)))
        }
        Parameter::TokenId => uuid(node).map(|value| ParameterValue::TokenId(TokenId(value))),
        Parameter::UserId => uuid(node).map(|value| ParameterValue::UserId(UserId(value))),
        Parameter::BeforeUuid => {
            if matches!(node, Node::Null) {
                Ok(ParameterValue::BeforeUuid(None))
            } else {
                uuid(node).map(|value| ParameterValue::BeforeUuid(Some(value)))
            }
        }
        Parameter::Limit => limit(node).map(ParameterValue::Limit),
        Parameter::Email => utf8_text(node, TextRule::Email).map(ParameterValue::Email),
        Parameter::BeforeString | Parameter::ReturnTo => {
            let value = if matches!(node, Node::Null) {
                Ok(None)
            } else {
                text(node, TextRule::Plain).map(Some)
            }?;
            Ok(if matches!(parameter, Parameter::BeforeString) {
                ParameterValue::BeforeString(value)
            } else {
                ParameterValue::ReturnTo(value)
            })
        }
        Parameter::Slug => text(node, TextRule::Plain).map(ParameterValue::Slug),
        Parameter::ConfigKind => match node {
            Node::String(value) if value.as_utf8().is_none() => Err(STRING_UNICODE),
            Node::String(value) if value == "science" => Ok(ParameterValue::ConfigKind("science")),
            Node::String(value) if value == "dashboard" => {
                Ok(ParameterValue::ConfigKind("dashboard"))
            }
            _ => Err(issue(
                "literal_error",
                "Input should be 'science' or 'dashboard'",
            )),
        },
        Parameter::ConfigRevision => query_integer(node).map(ParameterValue::ConfigRevision),
        Parameter::ConfigBefore => {
            if matches!(node, Node::Null) {
                return Ok(ParameterValue::ConfigBefore(None));
            }
            let parsed = Node::Integer(query_integer(node)?);
            let value = positive_i32_integer(&parsed)?;
            Ok(ParameterValue::ConfigBefore(Some(value)))
        }
    })();
    result.map_err(|error| ValidationErrors::one(parameter.location(), error))
}
/// Validate a required lossless `dict[str, Any]` after authentication.
/// # Errors
/// Preserves source missing/null and non-dictionary categories without projecting values.
pub fn validate_document_body(input: BodyInput<'_>) -> Checked<&Document> {
    match input {
        BodyInput::Missing => Err(ValidationErrors::one(
            vec![Location::field("body")],
            MISSING,
        )),
        BodyInput::Json(document) if matches!(document.node(document.root()), Some(Node::Null)) => {
            Err(ValidationErrors::one(
                vec![Location::field("body")],
                MISSING,
            ))
        }
        BodyInput::Json(document)
            if matches!(document.node(document.root()), Some(Node::Object(_))) =>
        {
            Ok(document)
        }
        _ => Err(ValidationErrors::one(
            vec![Location::field("body")],
            issue("dict_type", "Input should be a valid dictionary"),
        )),
    }
}
#[derive(Clone)]
pub struct FindUsersQuery {
    pub email: String,
    pub before: Option<Uuid>,
    pub limit: usize,
}
/// Bind decoded query pairs: last scalar occurrence wins; unrelated keys ignored.
/// # Errors
/// Errors follow endpoint signature order, not query input order.
pub fn find_users_query(pairs: &[(String, String)]) -> Checked<FindUsersQuery> {
    let find = |name: &str| {
        pairs
            .iter()
            .rev()
            .find(|(key, _)| key.equals_utf8(name))
            .map(|(_, value)| Node::String(value.clone()))
    };
    let mut problems = Vec::new();
    let mut parsed = |parameter: Parameter, result: Result<_, Issue>| match result {
        Ok(value) => Some(value),
        Err(error) => {
            problems.push(Problem {
                loc: parameter.location(),
                kind: error.kind,
                message: error.message,
            });
            None
        }
    };
    let email = parsed(
        Parameter::Email,
        find("email")
            .ok_or(MISSING)
            .and_then(|node| utf8_text(&node, TextRule::Email)),
    );
    let before = match find("before") {
        Some(node) => match uuid(&node) {
            Ok(value) => Some(Some(value)),
            Err(error) => {
                problems.push(Problem {
                    loc: Parameter::BeforeUuid.location(),
                    kind: error.kind,
                    message: error.message,
                });
                None
            }
        },
        None => Some(None),
    };
    let limit = match find("limit") {
        Some(node) => match limit(&node) {
            Ok(value) => Some(value),
            Err(error) => {
                problems.push(Problem {
                    loc: Parameter::Limit.location(),
                    kind: error.kind,
                    message: error.message,
                });
                None
            }
        },
        None => Some(50),
    };
    match (email, before, limit) {
        (Some(email), Some(before), Some(limit)) if problems.is_empty() => Ok(FindUsersQuery {
            email,
            before,
            limit,
        }),
        _ => Err(ValidationErrors { problems }),
    }
}

/// Validate a run submission request: one run document. The document itself
/// is parsed and checked against the run schema by the route.
/// # Errors
/// Returns ordered field errors followed by extras in insertion order.
pub fn validate_run_submission(input: BodyInput<'_>) -> Checked<String> {
    let mut fields = Fields::from_input(input)?;
    let document = fields.field("document", None, |node| {
        let value = utf8_text(node, TextRule::Plain)?;
        if value.is_empty() {
            return Err(issue(
                "string_too_short",
                "String should have at least 1 character",
            ));
        }
        Ok(value)
    });
    fields.extras(&["document"]);
    match document {
        Some(document) if fields.problems.is_empty() => Ok(document),
        _ => Err(fields.failure()),
    }
}
/// A brief revision: the whole document and the revision it replaces.
pub struct BriefRevise {
    pub document: String,
    pub expected_revision: i32,
}
/// Validate a brief revision request. The document itself is parsed and
/// checked against the brief schema by the route.
/// # Errors
/// Returns ordered field errors followed by extras in insertion order.
pub fn validate_brief_revise(input: BodyInput<'_>) -> Checked<BriefRevise> {
    let mut fields = Fields::from_input(input)?;
    let document = fields.field("document", None, |node| {
        let value = utf8_text(node, TextRule::Plain)?;
        if value.is_empty() {
            return Err(issue(
                "string_too_short",
                "String should have at least 1 character",
            ));
        }
        Ok(value)
    });
    let expected_revision = fields.field("expected_revision", None, |node| {
        let value = integer(node)?;
        if value < BigInt::from(0) {
            return Err(issue(
                "greater_than_equal",
                "Input should be greater than or equal to 0",
            ));
        }
        value.to_i32().ok_or(issue(
            "less_than_equal",
            "Input should be less than or equal to 2147483647",
        ))
    });
    fields.extras(&["document", "expected_revision"]);
    match (document, expected_revision) {
        (Some(document), Some(expected_revision)) if fields.problems.is_empty() => {
            Ok(BriefRevise {
                document,
                expected_revision,
            })
        }
        _ => Err(fields.failure()),
    }
}
/// Source `TrackUpdate` field order and explicitly supplied-field set.
pub struct TrackUpdate {
    pub expected_revision: BigInt,
    pub title: Option<String>,
    pub description: Option<String>,
    pub producer: Option<Document>,
    pub mode: Option<cannery_tracks::repo::TrackMode>,
    pub workflow: Option<Document>,
    pub reason: Option<String>,
    pub changed: BTreeSet<String>,
}
fn optional_text(node: &Node) -> Result<Option<String>, Issue> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        text(node, TextRule::Plain).map(Some)
    }
}
fn optional_track_mode(node: &Node) -> Result<Option<cannery_tracks::repo::TrackMode>, Issue> {
    match node {
        Node::Null => Ok(None),
        Node::String(value) => value.as_utf8().ok_or(STRING_UNICODE).and_then(|value| {
            cannery_tracks::repo::TrackMode::try_from(value.as_str())
                .map(Some)
                .map_err(|_| issue("literal_error", "Input should be 'agent' or 'workflow'"))
        }),
        _ => Err(issue(
            "literal_error",
            "Input should be 'agent' or 'workflow'",
        )),
    }
}
fn mapping(document: &Document, id: NodeId) -> Result<Option<Document>, Issue> {
    match document.node(id) {
        Some(Node::Null) => Ok(None),
        Some(Node::Object(_)) => {
            let mut builder = cannery_core::json::DocumentBuilder::new();
            let root = builder
                .import(document, id)
                .map_err(|_| issue("dict_type", "Input should be a valid dictionary"))?;
            builder
                .finish(root)
                .map(Some)
                .map_err(|_| issue("dict_type", "Input should be a valid dictionary"))
        }
        _ => Err(issue("dict_type", "Input should be a valid dictionary")),
    }
}
/// # Errors
/// Preserves source declaration-order coercion and trailing extra-field errors.
#[allow(
    clippy::too_many_lines,
    reason = "Source model field declaration order determines validation details"
)]
pub fn validate_track_update(input: BodyInput<'_>) -> Checked<TrackUpdate> {
    let document = match input {
        BodyInput::Missing => {
            return Err(ValidationErrors::one(
                vec![Location::field("body")],
                MISSING,
            ));
        }
        BodyInput::RawBytes => {
            return Err(ValidationErrors::one(
                vec![Location::field("body")],
                issue(
                    "model_attributes_type",
                    "Input should be a valid dictionary or object to extract fields from",
                ),
            ));
        }
        BodyInput::Json(document) => document,
    };
    let mut fields = Fields::new(document)?;
    let expected_revision = fields.field("expected_revision", None, integer);
    let title = fields.field("title", Some(None), optional_text);
    let description = fields.field("description", Some(None), optional_text);
    let mut dictionary = |name: &str| {
        let Some(id) = document.field(document.root(), name) else {
            return Some(None);
        };
        match mapping(document, id) {
            Ok(value) => Some(value),
            Err(error) => {
                fields.push(vec![Location::field("body"), Location::field(name)], error);
                None
            }
        }
    };
    let producer = dictionary("producer");
    let mode = fields.field("mode", Some(None), optional_track_mode);
    let workflow = match document.field(document.root(), "workflow") {
        None => Some(None),
        Some(id) => match mapping(document, id) {
            Ok(value) => Some(value),
            Err(error) => {
                fields.push(
                    vec![Location::field("body"), Location::field("workflow")],
                    error,
                );
                None
            }
        },
    };
    let reason = fields.field("reason", Some(None), optional_text);
    fields.extras(&[
        "expected_revision",
        "title",
        "description",
        "producer",
        "mode",
        "workflow",
        "reason",
    ]);
    if !fields.problems.is_empty() {
        return Err(fields.failure());
    }
    let changed = ["title", "description", "producer", "mode", "workflow"]
        .into_iter()
        .filter(|name| document.field(document.root(), name).is_some())
        .map(str::to_owned)
        .collect();
    match (
        expected_revision,
        title,
        description,
        producer,
        mode,
        workflow,
        reason,
    ) {
        (
            Some(expected_revision),
            Some(title),
            Some(description),
            Some(producer),
            Some(mode),
            Some(workflow),
            Some(reason),
        ) => Ok(TrackUpdate {
            expected_revision,
            title,
            description,
            producer,
            mode,
            workflow,
            reason,
            changed,
        }),
        _ => Err(fields.failure()),
    }
}

/// Ordered hypothesis path/query validation; cursors alone have INT4 bounds.
pub struct HypothesisParameters {
    pub number: Option<BigInt>,
    pub revision: Option<BigInt>,
    pub states: Option<Vec<cannery_hypotheses::repo::HypothesisState>>,
    pub archived: Option<bool>,
    pub track: Option<String>,
    pub before: Option<BigInt>,
    pub limit: usize,
}
/// # Errors
/// Reports path errors before signature-ordered query errors.
#[allow(
    clippy::too_many_lines,
    reason = "Preserve path and signature-ordered query error groups"
)]
pub fn hypothesis_parameters(
    number: Option<&str>,
    revision: Option<&str>,
    pairs: &[(String, String)],
    list: bool,
    revisions: bool,
) -> Checked<HypothesisParameters> {
    let mut errors = Vec::new();
    let mut parsed_integer = |raw: Option<&str>,
                              location: &str,
                              name: &str,
                              bounds: Option<(i32, i32)>| {
        raw.and_then(|raw| {
            match query_integer(&Node::String(String::from(raw))).and_then(|v| {
                if bounds.is_some_and(|(min, max)| v < BigInt::from(min) || v > BigInt::from(max)) {
                    Err(issue("bound", "Integer is outside the allowed range"))
                } else {
                    Ok(v)
                }
            }) {
                Ok(v) => Some(v),
                Err(issue) => {
                    errors.push(Problem {
                        loc: vec![Location::field(location), Location::field(name)],
                        kind: issue.kind,
                        message: issue.message,
                    });
                    None
                }
            }
        })
    };
    let number = parsed_integer(number, "path", "number", None);
    let revision = parsed_integer(revision, "path", "revision", None);
    let find = |name: &str| {
        pairs
            .iter()
            .rev()
            .find(|(k, _)| k.equals_utf8(name))
            .map(|(_, v)| v.clone())
    };
    // Finish path validation before the query groups below.
    let mut states = None;
    let mut archived = None;
    let mut track = None;
    if list {
        let raw: Vec<_> = pairs
            .iter()
            .filter(|(k, _)| k.equals_utf8("state"))
            .map(|(_, v)| v)
            .collect();
        if !raw.is_empty() {
            let mut result = Vec::new();
            for (index, v) in raw.iter().enumerate() {
                match v.as_utf8().and_then(|v| {
                    cannery_hypotheses::repo::HypothesisState::try_from(v.as_str()).ok()
                }) {
                    Some(v) => result.push(v),
                    None => errors.push(Problem {
                        loc: vec![
                            Location::field("query"),
                            Location::field("state"),
                            Location::Index(index),
                        ],
                        kind: "literal_error",
                        message: "Invalid hypothesis state",
                    }),
                }
            }
            states = Some(result);
        }
        if let Some(v) = find("archived") {
            archived = match v.as_utf8().map(|s| s.to_lowercase()).as_deref() {
                Some("1" | "true" | "t" | "yes" | "y" | "on") => Some(true),
                Some("0" | "false" | "f" | "no" | "n" | "off") => Some(false),
                _ => {
                    errors.push(Problem {
                        loc: vec![Location::field("query"), Location::field("archived")],
                        kind: "bool_parsing",
                        message: "Invalid boolean",
                    });
                    None
                }
            }
        }
        track = find("track");
    }
    let mut before = None;
    let mut limit = 50;
    if list || revisions {
        if let Some(v) = find("before") {
            match query_integer(&Node::String(v)).and_then(|v| {
                if v < BigInt::from(i32::from(list)) || v > BigInt::from(i32::MAX) {
                    Err(issue("bound", "Integer is outside the allowed range"))
                } else {
                    Ok(v)
                }
            }) {
                Ok(v) => before = Some(v),
                Err(issue) => errors.push(Problem {
                    loc: vec![Location::field("query"), Location::field("before")],
                    kind: issue.kind,
                    message: issue.message,
                }),
            }
        }
        if let Some(v) = find("limit") {
            match query_integer(&Node::String(v)).and_then(|v| {
                if v < BigInt::from(1) || v > BigInt::from(200) {
                    Err(issue("bound", "Integer is outside the allowed range"))
                } else {
                    Ok(v)
                }
            }) {
                Ok(v) => limit = v.to_usize().unwrap_or(50),
                Err(issue) => errors.push(Problem {
                    loc: vec![Location::field("query"), Location::field("limit")],
                    kind: issue.kind,
                    message: issue.message,
                }),
            }
        }
    }
    if errors.is_empty() {
        Ok(HypothesisParameters {
            number,
            revision,
            states,
            archived,
            track,
            before,
            limit,
        })
    } else {
        Err(ValidationErrors { problems: errors })
    }
}

/// Read-route signature parameters, with source path-before-query error order.
pub struct ReviewAttentionParameters {
    pub case_id: Option<cannery_core::ids::ReviewCaseId>,
    pub kind: Option<String>,
    pub state: Option<String>,
    pub before: Option<cannery_core::ids::ReviewCaseId>,
    pub limit: usize,
}
/// # Errors
/// Reports all signature-ordered model violations without inspecting project access.
pub fn review_attention_parameters(
    case_id: Option<&str>,
    pairs: &[(String, String)],
    list: bool,
    attention: bool,
) -> Checked<ReviewAttentionParameters> {
    let mut problems = Vec::new();
    let mut parse_uuid = |raw: Option<String>, location: &str, field: &str| {
        raw.and_then(|v| match uuid(&Node::String(v)) {
            Ok(v) => Some(cannery_core::ids::ReviewCaseId(v)),
            Err(e) => {
                problems.push(Problem {
                    loc: vec![Location::field(location), Location::field(field)],
                    kind: e.kind,
                    message: e.message,
                });
                None
            }
        })
    };
    let case_id = parse_uuid(case_id.map(String::from), "path", "case_id");
    let find = |name: &str| {
        pairs
            .iter()
            .rev()
            .find(|(k, _)| k.equals_utf8(name))
            .map(|(_, v)| v.clone())
    };
    // Query UUID follows kind/state, so collect its violations separately.
    let mut before = None;
    let mut kind = None;
    let mut state = None;
    if list {
        for (name, labels, result) in [
            ("kind", &["decision", "failure"][..], &mut kind),
            ("state", &["pending", "resolved"][..], &mut state),
        ] {
            if let Some(v) = find(name) {
                if labels.iter().any(|label| v.equals_utf8(label)) {
                    *result = Some(v);
                } else {
                    problems.push(Problem {
                        loc: vec![Location::field("query"), Location::field(name)],
                        kind: "literal_error",
                        message: "Input should be an allowed literal",
                    });
                }
            }
        }
        if let Some(v) = find("before") {
            match uuid(&Node::String(v)) {
                Ok(v) => before = Some(cannery_core::ids::ReviewCaseId(v)),
                Err(e) => problems.push(Problem {
                    loc: vec![Location::field("query"), Location::field("before")],
                    kind: e.kind,
                    message: e.message,
                }),
            }
        }
    }
    let mut limit = if attention { 10 } else { 50 };
    if (list || attention)
        && let Some(v) = find("limit")
    {
        match query_integer(&Node::String(v)).and_then(|v| {
            if v < BigInt::from(1) || v > BigInt::from(if attention { 50 } else { 200 }) {
                Err(issue("bound", "Integer is outside the allowed range"))
            } else {
                Ok(v)
            }
        }) {
            Ok(v) => limit = v.to_usize().unwrap_or(limit),
            Err(e) => problems.push(Problem {
                loc: vec![Location::field("query"), Location::field("limit")],
                kind: e.kind,
                message: e.message,
            }),
        }
    }
    if problems.is_empty() {
        Ok(ReviewAttentionParameters {
            case_id,
            kind,
            state,
            before,
            limit,
        })
    } else {
        Err(ValidationErrors { problems })
    }
}

/// Scalar query integer using checked signed 64-bit ASCII parsing.
pub(crate) fn bounded_query_integer(
    value: &str,
    name: &str,
    minimum: i64,
    maximum: Option<i64>,
) -> Checked<BigInt> {
    let location = vec![Location::field("query"), Location::field(name)];
    let parsed = query_integer(&Node::String(String::from(value)))
        .map_err(|error| ValidationErrors::one(location.clone(), error))?;
    if parsed < BigInt::from(minimum) {
        return Err(ValidationErrors::one(
            location,
            issue(
                "greater_than_equal",
                "Input should be greater than or equal to the minimum",
            ),
        ));
    }
    if maximum.is_some_and(|maximum| parsed > BigInt::from(maximum)) {
        return Err(ValidationErrors::one(
            location,
            issue(
                "less_than_equal",
                "Input should be less than or equal to the maximum",
            ),
        ));
    }
    Ok(parsed)
}

pub(crate) fn attempt_path_integer(name: &str, value: &str) -> Checked<BigInt> {
    query_integer(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(vec![Location::field("path"), Location::field(name)], error)
    })
}

pub(crate) fn attempt_header_integer(name: &str, value: &str) -> Checked<BigInt> {
    query_integer(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(
            vec![Location::field("header"), Location::field(name)],
            error,
        )
    })
}

pub(crate) fn model_integer(value: &Node) -> Option<BigInt> {
    integer(value).ok()
}
pub(crate) fn model_integer_at(value: &Node, location: Vec<Location>) -> Checked<BigInt> {
    integer(value).map_err(|error| ValidationErrors::one(location, error))
}
pub(crate) fn report_path_integer(name: &str, value: &str) -> Checked<BigInt> {
    query_integer(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(vec![Location::field("path"), Location::field(name)], error)
    })
}

pub(crate) fn artifact_uuid(value: &str) -> Checked<Uuid> {
    uuid(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(
            vec![Location::field("path"), Location::field("artifact_id")],
            error,
        )
    })
}
pub(crate) fn upload_uuid(value: &str) -> Checked<Uuid> {
    uuid(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(
            vec![Location::field("path"), Location::field("upload_id")],
            error,
        )
    })
}

/// Source claim declaration order, reusing the existing body and integer validators.
pub(crate) fn validate_claim_request(
    input: BodyInput<'_>,
) -> Checked<crate::claim_request::ClaimRequest> {
    let mut fields = Fields::from_input(input)?;
    let document = fields.document;
    let hypothesis = fields.field("hypothesis", Some(None), |node| {
        if matches!(node, Node::Null) {
            return Ok(None);
        }
        positive_i32_integer(node).map(Some)
    });
    let track = fields.field("track", Some(None), optional_text);
    let mode = fields.field("mode", Some(None), optional_track_mode);
    fields.extras(&["hypothesis", "track", "mode"]);
    match (hypothesis, track, mode) {
        (Some(hypothesis), Some(track), Some(mode)) if fields.problems.is_empty() => {
            Ok(crate::claim_request::ClaimRequest {
                hypothesis,
                track,
                mode,
                fields_set: ["hypothesis", "track", "mode"]
                    .iter()
                    .filter(|name| document.field(document.root(), name).is_some())
                    .map(|name| (*name).to_owned())
                    .collect(),
            })
        }
        _ => Err(fields.failure()),
    }
}
pub(crate) fn job_uuid(value: &str) -> Checked<Uuid> {
    uuid(&Node::String(String::from(value))).map_err(|error| {
        ValidationErrors::one(
            vec![Location::field("path"), Location::field("job_id")],
            error,
        )
    })
}
pub(crate) fn validate_job_claim(input: BodyInput<'_>) -> Checked<crate::job_claim_request::Claim> {
    let mut fields = Fields::from_input(input)?;
    let phase = fields.field("phase", Some(None), |node| match node {
        Node::Null => Ok(None),
        Node::String(value) if value.as_utf8().is_none() => Err(STRING_UNICODE),
        Node::String(value) if value.equals_utf8("verify") => {
            Ok(Some(cannery_jobs::repo::Phase::Verify))
        }
        Node::String(value) if value.equals_utf8("document") => {
            Ok(Some(cannery_jobs::repo::Phase::Document))
        }
        Node::String(value) if value.equals_utf8("decide") => {
            Ok(Some(cannery_jobs::repo::Phase::Decide))
        }
        _ => Err(issue(
            "literal_error",
            "Input should be 'verify', 'document' or 'decide'",
        )),
    });
    let revision = fields.field("revision", Some(None), |node| {
        if matches!(node, Node::Null) {
            return Ok(None);
        }
        utf8_text(node, TextRule::Bounded(1, 128)).map(Some)
    });
    fields.extras(&["phase", "revision"]);
    match (phase, revision) {
        (Some(phase), Some(revision)) if fields.problems.is_empty() => {
            Ok(crate::job_claim_request::Claim { phase, revision })
        }
        _ => Err(fields.failure()),
    }
}

pub(crate) fn job_input_key(value: Option<&str>) -> Checked<String> {
    let location = vec![Location::field("query"), Location::field("key")];
    value.map_or_else(
        || {
            Err(ValidationErrors::one(
                location.clone(),
                issue("missing", "Field required"),
            ))
        },
        |value| {
            text(
                &Node::String(String::from(value)),
                TextRule::Bounded(0, 1024),
            )
            .map_err(|error| ValidationErrors::one(location.clone(), error))
        },
    )
}

pub(crate) fn validate_job_upload(
    input: BodyInput<'_>,
) -> Checked<crate::job_upload_request::Request> {
    let mut fields = Fields::from_input(input)?;
    let role = fields.field("role", None, |node| upload_text(node, "role"));
    let path = fields.field("path", None, |node| {
        let path = utf8_text(node, TextRule::Plain)?;
        if path.len() > 512 {
            return Err(issue(
                "string_too_long",
                "String should have at most 512 characters",
            ));
        }
        if path.split('/').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        }) {
            return Err(issue(
                "string_pattern_mismatch",
                "String should match the upload path pattern",
            ));
        }
        Ok(path)
    });
    let size_bytes = fields.field("size_bytes", None, |node| {
        let n = integer(node)?;
        if n < BigInt::from(0) {
            Err(issue(
                "greater_than_equal",
                "Input should be greater than or equal to 0",
            ))
        } else {
            Ok(n)
        }
    });
    let sha256 = fields.field("sha256", None, |node| upload_text(node, "sha256"));
    let media_type = fields.field("media_type", None, |node| upload_text(node, "media_type"));
    let interface = fields.field("interface", Some(None), |node| {
        if matches!(node, Node::Null) {
            return Ok(None);
        }
        let value = utf8_text(node, TextRule::Plain)?;
        let valid = value.split_once("/v").is_some_and(|(name, version)| {
            !name.is_empty()
                && name.len() <= 63
                && name
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && name
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && (1..=2).contains(&version.split('.').count())
                && version
                    .split('.')
                    .all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
        });
        if valid {
            Ok(Some(value))
        } else {
            Err(issue(
                "string_pattern_mismatch",
                "String should match a registered interface reference",
            ))
        }
    });
    fields.extras(&[
        "role",
        "path",
        "size_bytes",
        "sha256",
        "media_type",
        "interface",
    ]);
    match (role, path, size_bytes, sha256, media_type, interface) {
        (
            Some(role),
            Some(path),
            Some(size_bytes),
            Some(sha256),
            Some(media_type),
            Some(interface),
        ) if fields.problems.is_empty() => Ok(crate::job_upload_request::Request {
            role,
            path,
            size_bytes,
            sha256,
            media_type,
            interface,
        }),
        _ => Err(fields.failure()),
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod native_integer_tests {
    use super::*;

    #[test]
    fn transport_integer_text_and_json_integers_use_separate_rules() {
        assert_eq!(
            attempt_path_integer("number", "1").ok(),
            Some(BigInt::from(1))
        );
        assert_eq!(
            attempt_header_integer("revision", "2").ok(),
            Some(BigInt::from(2))
        );
        assert_eq!(
            report_path_integer("number", "3").ok(),
            Some(BigInt::from(3))
        );
        assert!(bounded_query_integer("20", "limit", 1, Some(200)).is_ok());
        assert!(hypothesis_parameters(Some("1"), Some("2"), &[], false, false).is_ok());
        for raw in ["1_0", "١", "1.0", "9223372036854775808"] {
            assert!(attempt_path_integer("number", raw).is_err());
            assert!(attempt_header_integer("revision", raw).is_err());
            assert!(bounded_query_integer(raw, "limit", 1, None).is_err());
        }
        assert_eq!(
            model_integer(&Node::Integer(BigInt::from(1))),
            Some(BigInt::from(1))
        );
        assert!(model_integer(&Node::String("1".to_owned())).is_none());
        assert!(model_integer(&Node::Bool(true)).is_none());
        assert!(model_integer(&Node::Float(1.0)).is_none());
    }
}
