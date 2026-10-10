//! Review persistence; caller owns authorization, transactions and connection history.
#![forbid(unsafe_code)]
pub mod binding;
pub mod repo;
use cannery_core::json::Document;
use std::fmt;
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub decode_nesting_budget: usize,
}
pub enum Error {
    Database { sqlstate: Option<String> },
    Driver,
    TextEncoding,
    Decode(cannery_core::json::DecodeError),
    Invariant,
}
impl Error {
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
}
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(|e| e.code().map(std::borrow::Cow::into_owned)),
        }
    }
}
impl From<cannery_core::json::DecodeError> for Error {
    fn from(e: cannery_core::json::DecodeError) -> Self {
        Self::Decode(e)
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database { sqlstate } => f
                .debug_struct("Database")
                .field("sqlstate", sqlstate)
                .finish(),
            Self::Driver => f.write_str("Driver"),
            Self::TextEncoding => f.write_str("TextEncoding"),
            Self::Decode(_) => f.write_str("Decode"),
            Self::Invariant => f.write_str("Invariant"),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
/// # Errors
/// Sanitizes source UTF-8 encoding and embedded-NUL binding failures.
pub fn text(value: &String) -> Result<String, Error> {
    let s = value.as_utf8().ok_or(Error::TextEncoding)?;
    if s.contains('\0') {
        return Err(Error::Driver);
    }
    Ok(s)
}
/// # Errors
/// Applies only the caller's explicitly calibrated decode budget.
pub fn document(value: &str, context: JsonContext) -> Result<Document, Error> {
    Ok(cannery_core::json::decode_str(
        value,
        context.decode_nesting_budget,
    )?)
}
macro_rules! labels{($name:ident{$($variant:ident=>$label:literal),+})=>{
#[derive(Clone,Copy,Debug,Eq,PartialEq,Ord,PartialOrd)] pub enum $name{$($variant),+}
impl $name{#[must_use]pub const fn as_str(self)->&'static str{match self{$(Self::$variant=>$label),+}}}
impl TryFrom<&str> for $name{type Error=Error;fn try_from(s:&str)->Result<Self,Error>{match s{$($label=>Ok(Self::$variant)),+,_=>Err(Error::Invariant)}}}
};}
labels!(CaseKind{Result=>"result",Failure=>"failure"});
labels!(CaseState{Pending=>"pending",Resolved=>"resolved"});
labels!(Origin{Live=>"live",Imported=>"imported"});
labels!(Stage{Agent=>"agent",Tester=>"tester",Evaluator=>"evaluator"});
labels!(HypothesisState{Queued=>"queued",Active=>"active",AwaitingHumanReview=>"awaiting_human_review",Promoted=>"promoted",Rejected=>"rejected",Inconclusive=>"inconclusive",Failed=>"failed",Cancelled=>"cancelled"});
labels!(AttemptState{Claimed=>"claimed",Running=>"running",Submitted=>"submitted",Testing=>"testing",Evaluating=>"evaluating",AwaitingHumanReview=>"awaiting_human_review",Promoted=>"promoted",Rejected=>"rejected",Inconclusive=>"inconclusive",Failed=>"failed",Cancelled=>"cancelled",Unreviewed=>"unreviewed"});
labels!(Action{Promote=>"promote",Reject=>"reject",Inconclusive=>"inconclusive",Retry=>"retry",CloseFailed=>"close_failed"});
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct FailureId(pub uuid::Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct EvidenceId(pub uuid::Uuid);

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
