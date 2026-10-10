//! Project schemas and instances validated by the stock JSON Schema engine.
use crate::json::{self, Document};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InstanceError {
    #[error("invalid JSON representation")]
    Value,
    #[error("project schema cannot be compiled")]
    Reference,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectViolation {
    pub path: String,
    pub message: String,
}
pub struct ProjectValidator {
    validator: jsonschema::Validator,
}
impl ProjectValidator {
    /// # Errors
    /// Rejects schemas outside application policy and unresolved references.
    pub fn new(schema: &Document) -> Result<Self, InstanceError> {
        let schema = json::to_value(schema).map_err(|_| InstanceError::Value)?;
        if !super::project_schema_errors(&schema).is_empty() {
            return Err(InstanceError::Reference);
        }
        let validator = super::formats::options()
            .build(&schema)
            .map_err(|_| InstanceError::Reference)?;
        Ok(Self { validator })
    }
    /// # Errors
    /// Rejects invalid document representations.
    pub fn violations(
        &self,
        document: &Document,
        limit: &BigInt,
    ) -> Result<Vec<ProjectViolation>, InstanceError> {
        if limit <= &BigInt::from(0) {
            return Ok(Vec::new());
        }
        let value = json::to_value(document).map_err(|_| InstanceError::Value)?;
        let limit = limit.to_usize().unwrap_or(1000).min(1000);
        Ok(super::schema_errors(&self.validator, &value)
            .into_iter()
            .take(limit)
            .map(|error| ProjectViolation {
                path: error.path,
                message: error.message,
            })
            .collect())
    }
}
#[derive(Debug)]
pub enum ProjectValidationFailure {
    Schema(Vec<super::ContractViolation>),
    Document(Vec<ProjectViolation>),
    Exception(InstanceError),
}
/// # Errors
/// Separates unusable schemas from ordinary instance violations.
pub fn validate_project_fields(
    schema: &Document,
    document: &Document,
) -> Result<(), ProjectValidationFailure> {
    let value = json::to_value(schema)
        .map_err(|_| ProjectValidationFailure::Exception(InstanceError::Value))?;
    let errors = super::project_schema_errors(&value);
    if !errors.is_empty() {
        return Err(ProjectValidationFailure::Schema(errors));
    }
    let validator = ProjectValidator::new(schema).map_err(ProjectValidationFailure::Exception)?;
    let violations = validator
        .violations(document, &BigInt::from(1000))
        .map_err(ProjectValidationFailure::Exception)?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(ProjectValidationFailure::Document(violations))
    }
}
