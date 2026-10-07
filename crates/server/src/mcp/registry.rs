//! Frozen production tool metadata, with maintained JSON Schema validation.
use super::StartupError;
use axum::http::{HeaderMap, HeaderValue, Method, Uri};
use serde_json::{Map, Value, json};
use std::fmt::Write;

pub(super) struct Tool {
    pub definition: Value,
    pub validator: jsonschema::Validator,
}
impl Tool {
    pub fn name(&self) -> &str {
        self.definition["name"].as_str().unwrap_or_default()
    }
    pub fn invalid_arguments(&self, args: &Value) -> Option<Value> {
        let errors: Vec<_> = self.validator.iter_errors(args).map(|error| {
            // Do not put values (possibly lease tokens) into diagnostic messages.
            json!({"path": error.instance_path().to_string(), "message": "argument does not match the tool input schema"})
        }).collect();
        (!errors.is_empty()).then(|| json!(errors))
    }
    #[allow(
        clippy::too_many_lines,
        reason = "All 42 canonical tool mappings are reviewed together"
    )]
    pub fn request(&self, args: &Map<String, Value>) -> Result<ToolRequest, ()> {
        let mut args = args.clone();
        let name = self.name();
        let project = if name == "search" {
            String::new()
        } else {
            take_path(&mut args, "project")
        };
        let base = format!("/api/projects/{project}");
        let mut headers = HeaderMap::new();
        for (argument, header) in [
            ("lease_token", "x-lease-token"),
            ("lease_generation", "x-lease-generation"),
            ("idempotency_key", "idempotency-key"),
        ] {
            if let Some(value) = args.remove(argument).filter(|v| !v.is_null()) {
                let text = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                if !text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
                    return Err(());
                }
                headers.insert(header, HeaderValue::from_str(&text).map_err(|_| ())?);
            }
        }
        let (method, path) = match name {
            "list_projects" => (Method::GET, "/api/projects".into()),
            "list_tracks" => (Method::GET, format!("{base}/tracks")),
            "get_track" | "update_track" | "track_history" | "transition_track" => {
                let track = take_path(&mut args, "track");
                let (method, suffix) = match name {
                    "update_track" => (Method::PATCH, ""),
                    "track_history" => (Method::GET, "/history"),
                    "transition_track" => (Method::POST, "/transitions"),
                    _ => (Method::GET, ""),
                };
                (method, format!("{base}/tracks/{track}{suffix}"))
            }
            "create_track" => (Method::POST, format!("{base}/tracks")),
            "create_draft" | "list_hypotheses" => (
                if name == "create_draft" {
                    Method::POST
                } else {
                    Method::GET
                },
                format!("{base}/hypotheses"),
            ),
            "get_hypothesis" | "revise_draft" | "list_hypothesis_revisions" | "review_draft" => {
                let number = take_path(&mut args, "number");
                let (method, suffix) = match name {
                    "revise_draft" => (Method::PUT, ""),
                    "list_hypothesis_revisions" => (Method::GET, "/revisions"),
                    "review_draft" => (Method::POST, "/draft-review"),
                    _ => (Method::GET, ""),
                };
                (method, format!("{base}/hypotheses/{number}{suffix}"))
            }
            "claim_hypothesis" => (Method::POST, format!("{base}/claims")),
            "list_attempts" => (Method::GET, format!("{base}/attempts")),
            "get_attempt" | "heartbeat_attempt" | "release_attempt" | "create_upload"
            | "post_manifest" | "submit_attempt" | "get_report" | "list_attempt_jobs" => {
                let number = take_path(&mut args, "number");
                let sequence = take_path(&mut args, "sequence");
                let (method, suffix) = match name {
                    "heartbeat_attempt" => (Method::POST, "/heartbeat"),
                    "release_attempt" => (Method::POST, "/release"),
                    "create_upload" => (Method::POST, "/uploads"),
                    "post_manifest" => (Method::POST, "/manifest"),
                    "submit_attempt" => (Method::POST, "/submission"),
                    "get_report" => (Method::GET, "/report"),
                    "list_attempt_jobs" => (Method::GET, "/jobs"),
                    _ => (Method::GET, ""),
                };
                (
                    method,
                    format!("{base}/hypotheses/{number}/attempts/{sequence}{suffix}"),
                )
            }
            "claim_job" => (Method::POST, format!("{base}/jobs/claims")),
            "get_job" | "heartbeat_job" | "get_job_input" | "create_job_upload"
            | "complete_job" | "fail_job" => {
                let id = take_path(&mut args, "job_id");
                let (method, suffix) = match name {
                    "heartbeat_job" => (Method::POST, "/heartbeat".into()),
                    "get_job_input" => (
                        Method::GET,
                        format!(
                            "/inputs/{}",
                            take_path(&mut args, "input").replace('_', "-")
                        ),
                    ),
                    "create_job_upload" => (Method::POST, "/uploads".into()),
                    "complete_job" => (Method::POST, "/completion".into()),
                    "fail_job" => (Method::POST, "/failure".into()),
                    _ => (Method::GET, String::new()),
                };
                (method, format!("{base}/jobs/{id}{suffix}"))
            }
            "list_review_cases" => (Method::GET, format!("{base}/review-cases")),
            "get_review_case" | "record_decision" => {
                let id = take_path(&mut args, "case_id");
                if name == "record_decision" {
                    args.insert("review_case_id".into(), Value::String(id.clone()));
                    if args.get("supersedes").is_some_and(Value::is_null) {
                        args.remove("supersedes");
                    }
                }
                (
                    if name == "record_decision" {
                        Method::POST
                    } else {
                        Method::GET
                    },
                    format!(
                        "{base}/review-cases/{id}{}",
                        if name == "record_decision" {
                            "/decisions"
                        } else {
                            ""
                        }
                    ),
                )
            }
            "list_reports" => (Method::GET, format!("{base}/reports")),
            "comment" | "list_comments" => {
                let number = take_path(&mut args, "number");
                let sequence = args
                    .remove("sequence")
                    .filter(|v| !v.is_null())
                    .map(|v| format!("/attempts/{}", encode(&v.to_string())))
                    .unwrap_or_default();
                (
                    if name == "comment" {
                        Method::POST
                    } else {
                        Method::GET
                    },
                    format!("{base}/hypotheses/{number}{sequence}/comments"),
                )
            }
            "edit_comment" => {
                let id = take_path(&mut args, "comment_id");
                (Method::PUT, format!("{base}/comments/{id}"))
            }
            "search" => (Method::GET, "/api/search".into()),
            "metric_catalog" => (Method::GET, format!("{base}/metrics")),
            "query_metrics" => (Method::GET, format!("{base}/metrics/query")),
            "query_comparisons" => (Method::GET, format!("{base}/comparisons")),
            // get_artifact has a typed direct metadata service, never a byte GET.
            _ => return Err(()),
        };
        let (uri, body) = if method == Method::GET {
            if let Some(properties) = self.definition["inputSchema"]["properties"].as_object() {
                for (key, schema) in properties {
                    if key != "project"
                        && !args.contains_key(key)
                        && let Some(default) = schema.get("default").filter(|v| !v.is_null())
                    {
                        args.insert(key.clone(), default.clone());
                    }
                }
            }
            let mut query = Vec::new();
            for (key, value) in args {
                for value in value
                    .as_array()
                    .map_or_else(|| vec![&value], |values| values.iter().collect())
                {
                    if !value.is_null() {
                        let text = value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned);
                        query.push(format!("{}={}", encode(&key), encode(&text)));
                    }
                }
            }
            (
                format!(
                    "{path}{}{}",
                    if query.is_empty() { "" } else { "?" },
                    query.join("&")
                ),
                Vec::new(),
            )
        } else {
            let body = if matches!(
                name,
                "create_track"
                    | "transition_track"
                    | "create_draft"
                    | "post_manifest"
                    | "submit_attempt"
                    | "complete_job"
                    | "fail_job"
            ) {
                args.remove("document").ok_or(())?
            } else {
                Value::Object(args)
            };
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            (path, serde_json::to_vec(&body).map_err(|_| ())?)
        };
        Ok(ToolRequest {
            method,
            uri: uri.parse().map_err(|_| ())?,
            headers,
            body,
        })
    }
}

pub(super) struct ToolRequest {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
fn take_path(args: &mut Map<String, Value>, name: &str) -> String {
    args.remove(name)
        .filter(|v| !v.is_null())
        .map(|v| encode(&v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
        .unwrap_or_default()
}
pub(super) fn encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}
pub(super) fn tools() -> Result<Vec<Tool>, StartupError> {
    let values: Vec<Value> =
        serde_json::from_str(include_str!("tools.json")).map_err(|_| StartupError::Registry)?;
    if values.len() != 42 {
        return Err(StartupError::Registry);
    }
    values
        .into_iter()
        .map(|mut definition| {
            for name in ["lease_token", "idempotency_key"] {
                if let Some(property) = definition["inputSchema"]["properties"].get_mut(name) {
                    property["pattern"] = json!("^[\\x20-\\x7e]+$");
                }
            }
            let validator = jsonschema::options()
                .should_validate_formats(true)
                .build(&definition["inputSchema"])
                .map_err(|_| StartupError::Registry)?;
            definition["outputSchema"] = json!({"type":"object","additionalProperties":true});
            Ok(Tool {
                definition,
                validator,
            })
        })
        .collect()
}
