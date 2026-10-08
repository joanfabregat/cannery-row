// SPDX-License-Identifier: AGPL-3.0-only
//! The front matter schema of each phase output, public like the API
//! document: editors and agents validate a document against it before
//! submitting.
use crate::{
    ServerError,
    attempt_lease_routes::{Failure, domain},
};
use axum::{
    Router,
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    contracts::phases::{Phase, PhaseSchemas},
    errors::ErrorCode,
};
use std::sync::Arc;

/// The bundled schemas are serialized once, at startup.
pub(crate) fn routes() -> Result<Router, ServerError> {
    let schemas = PhaseSchemas::new().map_err(|_| ServerError::Contracts)?;
    let documents = Phase::ALL
        .into_iter()
        .map(|phase| {
            serde_json::to_vec(schemas.schema(phase))
                .map(|bytes| (phase, bytes))
                .map_err(|_| ServerError::Contracts)
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    Ok(Router::new()
        .route("/api/schemas/{phase}", get(schema))
        .with_state(Arc::new(documents)))
}

#[utoipa::path(
    get,
    path = "/api/schemas/{phase}",
    operation_id = "phase_schema_api_schemas__phase__get",
    summary = "Phase Schema",
    description = "The JSON Schema of a phase output's front matter, as one self-contained\ndocument: the published schemas it references are embedded under `$defs`.\nNo authentication.",
    params(("phase" = crate::api_models::DocumentPhase, Path)),
    responses((status = 200, description = "Successful Response", body = serde_json::Value, content_type = "application/schema+json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn schema(
    State(documents): State<Arc<std::collections::BTreeMap<Phase, Vec<u8>>>>,
    Path(phase): Path<String>,
) -> Result<Response, Failure> {
    let document = Phase::from_name(&phase)
        .and_then(|phase| documents.get(&phase))
        .ok_or_else(|| domain(ErrorCode::NotFound, "unknown phase"))?;
    Ok((
        [(header::CONTENT_TYPE, "application/schema+json")],
        document.clone(),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    async fn get(path: &str) -> Result<(StatusCode, Option<String>, serde_json::Value)> {
        let response = routes()?
            .oneshot(Request::builder().uri(path).body(Body::empty())?)
            .await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), 1 << 20).await?;
        Ok((status, content_type, serde_json::from_slice(&bytes)?))
    }

    #[tokio::test]
    async fn every_phase_serves_its_bundled_schema() -> Result {
        let schemas = PhaseSchemas::new()?;
        for phase in Phase::ALL {
            let (status, content_type, body) =
                get(&format!("/api/schemas/{}", phase.name())).await?;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(content_type.as_deref(), Some("application/schema+json"));
            assert_eq!(&body, schemas.schema(phase));
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_phase_is_not_found() -> Result {
        for path in [
            "/api/schemas/plan",
            "/api/schemas/Run",
            "/api/schemas/evidence_envelope",
        ] {
            let (status, _, body) = get(path).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(body["error"]["code"], "not_found", "{path}");
        }
        Ok(())
    }
}
