//! Source claim body model; transport and worker authorization stay in the controller.
use crate::validation::{BodyInput, ValidationErrors};
use cannery_jobs::repo::Stage;
pub(crate) struct Claim {
    pub stage: Option<Stage>,
    pub revision: Option<String>,
}
pub(crate) fn claim(input: BodyInput<'_>) -> Result<Claim, ValidationErrors> {
    crate::validation::validate_job_claim(input)
}
