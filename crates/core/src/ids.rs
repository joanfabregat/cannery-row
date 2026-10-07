//! Distinct identifiers prevent mixing entities at domain boundaries.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! identifiers {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, sqlx::Type)]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    )+};
}

identifiers!(
    UserId,
    ProjectId,
    ServiceAccountId,
    SessionId,
    TokenId,
    TrackId,
    HypothesisId,
    AttemptId,
    JobId,
    ReportId,
    ReviewCaseId,
);
