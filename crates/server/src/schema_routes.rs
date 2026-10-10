// SPDX-License-Identifier: AGPL-3.0-only
//! Every published contract schema by name, public like the API document:
//! editors and agents validate a document against it before submitting.
use crate::{
    ServerError,
    attempt_lease_routes::{Failure, failure},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    contracts::published,
    errors::{DomainError, ErrorCode},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// What each published schema describes, for the list.
fn about(name: &str) -> &'static str {
    match name {
        "artifact_manifest" => {
            "The artifact manifest an attempt or a job records (post_manifest, complete_job): the verified uploads, each with its role, storage, size, digest and media type."
        }
        "brief" => "The front matter of a project's brief (revise_brief).",
        "common" => {
            "Shared definitions the other schemas reference: slugs, names, identifiers, digests, timestamps."
        }
        "concern" => "The front matter of a concern about a track's plan (raise_concern).",
        "dashboard_views" => "A project's dashboard views configuration.",
        "decision" => "The front matter of a decision document (record_decision, a decide job).",
        "evidence_envelope" => {
            "A claimed result sheet from before run documents; its measurement, discrepancy and comparison definitions are shared with the run and verification schemas."
        }
        "gates" => "A verifier policy's gates.",
        "human_decision" => "A researcher's decision on a review case, as recorded.",
        "import_bundle" => "A bundle of earlier research imported into a project.",
        "interface" => "An interface: the format of an artifact a step reads or writes.",
        "job" => "A job as a runner claims it.",
        "job_completion" => {
            "What completes a job (complete_job): the document and, for a verify job, its manifest."
        }
        "job_failure" => "What fails a job (fail_job): the error code, reason and logs.",
        "policy_config" => "A verifier policy's configuration.",
        "run" => {
            "The front matter of a run document (submit_attempt): claims, provenance, artifact roles and the verified manifest."
        }
        "science_revision" => {
            "A project's science revision: metrics, datasets, interfaces, required artifact roles, limits."
        }
        "step_manifest" => "A step manifest: a producer, scorer, verifier or experiment step.",
        "track" => "A track as create_track takes it.",
        "track_transition" => "A track state change (transition_track).",
        "transcript" => "One event of an attempt's transcript (append_transcript).",
        "unit" => {
            "A unit of a track plan as add_unit takes it, with its acceptance: splits, primary metric, required slices, criteria, regression gates and compute budget."
        }
        "verification" => {
            "The front matter of a verification report (complete_job on a verify job)."
        }
        "writeup" => "The front matter of a write-up (complete_job on a document job, write_up).",
        _ => "",
    }
}

pub(crate) struct Schemas {
    documents: BTreeMap<&'static str, Vec<u8>>,
    list: Value,
}

/// The bundled schemas are serialized once, at startup.
pub(crate) fn routes() -> Result<Router, ServerError> {
    let documents = published::names()
        .into_iter()
        .map(|name| {
            published::bundled(name)
                .ok()
                .and_then(|schema| serde_json::to_vec(&schema).ok())
                .map(|bytes| (name, bytes))
                .ok_or(ServerError::Contracts)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let list = json!({"items": documents.keys().map(|name| json!({
        "name": name,
        "ref": format!("/api/schemas/{name}"),
        "description": about(name),
    })).collect::<Vec<_>>()});
    Ok(Router::new()
        .route("/api/schemas", get(list_schemas))
        .route("/api/schemas/{name}", get(schema))
        .with_state(Arc::new(Schemas { documents, list })))
}

#[utoipa::path(
    get,
    path = "/api/schemas",
    operation_id = "list_schemas_api_schemas_get",
    summary = "List Schemas",
    description = "The name, reference and a one-line description of every published contract\nschema: the documents the API takes (unit and its acceptance, manifest,\nrun, verification, write-up, decision, concern, transcript, brief, ...).\nNo authentication.",
    responses((status = 200, description = "Successful Response", body = crate::api_models::SchemaListOut, content_type = "application/json"))
)]
pub(crate) async fn list_schemas(State(schemas): State<Arc<Schemas>>) -> Response {
    Json(schemas.list.clone()).into_response()
}

#[utoipa::path(
    get,
    path = "/api/schemas/{name}",
    operation_id = "contract_schema_api_schemas__name__get",
    summary = "Contract Schema",
    description = "A published contract schema by name (`GET /api/schemas` lists them), as one\nself-contained JSON Schema document: the published schemas it references\nare embedded under `$defs`. For a phase output (`brief`, `run`,\n`verification`, `writeup`, `decision`, `concern`) it is the schema of the\ndocument's front matter. No authentication.",
    params(("name" = crate::api_models::ContractSchemaName, Path)),
    responses((status = 200, description = "Successful Response", body = serde_json::Value, content_type = "application/schema+json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn schema(
    State(schemas): State<Arc<Schemas>>,
    Path(name): Path<String>,
) -> Result<Response, Failure> {
    let document = schemas.documents.get(name.as_str()).ok_or_else(|| {
        failure(crate::errors::ApiError::from(
            DomainError::new(
                ErrorCode::NotFound,
                "unknown schema; GET /api/schemas lists the published names",
            )
            .with_details(json!({"names": schemas.documents.keys().collect::<Vec<_>>()})),
        ))
    })?;
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
    use cannery_core::contracts::phases::{Phase, PhaseSchemas};
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
    async fn every_published_schema_is_served_and_listed() -> Result {
        let (status, _, list) = get("/api/schemas").await?;
        assert_eq!(status, StatusCode::OK);
        let items = list["items"].as_array().ok_or("items")?;
        assert_eq!(items.len(), published::names().len());
        for item in items {
            let name = item["name"].as_str().ok_or("name")?;
            assert!(!about(name).is_empty(), "{name} lacks a description");
            let (status, content_type, body) = get(&format!("/api/schemas/{name}")).await?;
            assert_eq!(status, StatusCode::OK, "{name}");
            assert_eq!(content_type.as_deref(), Some("application/schema+json"));
            assert_eq!(body, published::bundled(name)?);
        }
        let (_, _, manifest) = get("/api/schemas/artifact_manifest").await?;
        assert_eq!(manifest["title"], "Artifact manifest");
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_name_is_not_found_and_lists_the_names() -> Result {
        for path in [
            "/api/schemas/plan",
            "/api/schemas/Run",
            "/api/schemas/manifest",
        ] {
            let (status, _, body) = get(path).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(body["error"]["code"], "not_found", "{path}");
            assert!(
                body["error"]["details"]["names"]
                    .as_array()
                    .is_some_and(|names| names.contains(&json!("artifact_manifest"))),
                "{path}"
            );
        }
        Ok(())
    }
}
