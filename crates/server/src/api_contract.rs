//! Shared serde conversions for REST DTO adapters.
use cannery_core::json::model::ModelEncodeError;
use serde::{Serialize, de::DeserializeOwned};

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ModelEncodeError> {
    serde_json::to_vec(value).map_err(|_| ModelEncodeError::Encoding)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ModelEncodeError> {
    serde_json::from_slice(bytes).map_err(|_| ModelEncodeError::InvalidNode)
}

/// Convert a domain enum or identifier to its public DTO representation.
pub(crate) fn convert<T: DeserializeOwned>(value: impl Serialize) -> Result<T, ModelEncodeError> {
    serde_json::from_value(serde_json::to_value(value).map_err(|_| ModelEncodeError::Encoding)?)
        .map_err(|_| ModelEncodeError::InvalidNode)
}

/// Decode the declared REST DTO before domain validation. Preserve field presence
/// for patch semantics while carrying serde-decoded values into the domain AST.
pub(crate) fn typed_body<T: DeserializeOwned + Serialize>(
    body: crate::body::DecodedBody,
) -> Result<crate::body::DecodedBody, crate::errors::ApiError> {
    use crate::body::{DecodedBody, REST_JSON_NESTING_BUDGET};
    use cannery_core::{
        errors::{DomainError, ErrorCode},
        json::{self, model},
    };
    let DecodedBody::Json(document) = body else {
        return Ok(body);
    };
    let error = || {
        crate::errors::ApiError::from(DomainError::new(
            ErrorCode::ValidationFailed,
            "request does not match the REST contract",
        ))
    };
    let bytes = model::encode_inferred(&document, document.root(), REST_JSON_NESTING_BUDGET)
        .map_err(|_| error())?;
    let original: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| error())?;
    if !original.is_object() && !original.is_null() {
        return Err(error());
    }
    let typed: T = serde_json::from_value(original.clone()).map_err(|_| error())?;
    let mut value = serde_json::to_value(typed).map_err(|_| error())?;
    if let (Some(original), Some(converted)) = (original.as_object(), value.as_object_mut()) {
        converted.retain(|key, _| original.contains_key(key));
        for (key, old_value) in original {
            converted
                .entry(key.clone())
                .or_insert_with(|| old_value.clone());
        }
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| error())?;
    Ok(DecodedBody::Json(
        json::decode(&bytes, REST_JSON_NESTING_BUDGET).map_err(|_| error())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api_models, body::DecodedBody};
    use cannery_core::json::{self, Node};

    #[test]
    fn typed_patch_preserves_missing_and_explicit_null() -> Result<(), Box<dyn std::error::Error>> {
        let input = json::decode(br#"{"expected_revision":2,"producer":null}"#, 32)?;
        let output = typed_body::<api_models::TrackUpdate>(DecodedBody::Json(input))?;
        let DecodedBody::Json(document) = output else {
            return Err("missing JSON body".into());
        };
        assert!(document.field(document.root(), "title").is_none());
        let producer = document
            .field(document.root(), "producer")
            .ok_or("lost explicit null")?;
        assert!(matches!(document.node(producer), Some(Node::Null)));
        Ok(())
    }

    #[test]
    fn typed_control_request_rejects_wrong_field_types_and_unknown_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        for bytes in [
            br#"{"body_markdown":42}"#.as_slice(),
            br#"{"body_markdown":"comment","unknown":true}"#.as_slice(),
        ] {
            let document = json::decode(bytes, 32)?;
            assert!(typed_body::<api_models::CommentCreate>(DecodedBody::Json(document)).is_err());
        }
        Ok(())
    }

    #[test]
    fn typed_requests_reject_positional_arrays() -> Result<(), Box<dyn std::error::Error>> {
        for bytes in [br"[]".as_slice(), br#"["tester",null]"#.as_slice()] {
            let document = json::decode(bytes, 32)?;
            assert!(
                typed_body::<api_models::JobClaimRequest>(DecodedBody::Json(document)).is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn nullable_optional_envelopes_accept_explicit_null() -> Result<(), Box<dyn std::error::Error>>
    {
        let document = json::decode(br"null", 32)?;
        assert!(
            typed_body::<Option<api_models::PresignRequest>>(DecodedBody::Json(document)).is_ok()
        );
        Ok(())
    }
}
