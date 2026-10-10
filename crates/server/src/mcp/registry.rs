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
        reason = "All 77 canonical tool mappings are reviewed together"
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
            "get_brief" => {
                let revision = take_path(&mut args, "revision");
                if revision.is_empty() {
                    (Method::GET, format!("{base}/brief"))
                } else {
                    (Method::GET, format!("{base}/brief/revisions/{revision}"))
                }
            }
            "revise_brief" => (Method::POST, format!("{base}/brief")),
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
            "start_plan_revision"
            | "get_plan"
            | "list_plan_revisions"
            | "set_plan_approach"
            | "add_unit"
            | "update_unit"
            | "drop_unit"
            | "set_alignment"
            | "answer_concern"
            | "check_plan"
            | "submit_plan"
            | "review_plan"
            | "list_track_units" => {
                let track = take_path(&mut args, "track");
                let plans = format!("{base}/tracks/{track}/plans");
                match name {
                    "start_plan_revision" => (Method::POST, plans),
                    "list_plan_revisions" => (Method::GET, plans),
                    "get_plan" => {
                        let revision = take_path(&mut args, "revision");
                        let revision = if revision.is_empty() {
                            String::from("current")
                        } else {
                            revision
                        };
                        (Method::GET, format!("{plans}/{revision}"))
                    }
                    "set_plan_approach" => (Method::PUT, format!("{plans}/draft/approach")),
                    "add_unit" => (Method::POST, format!("{plans}/draft/units")),
                    "update_unit" | "drop_unit" => {
                        let key = take_path(&mut args, "key");
                        (
                            if name == "update_unit" {
                                Method::PUT
                            } else {
                                Method::DELETE
                            },
                            format!("{plans}/draft/units/{key}"),
                        )
                    }
                    "set_alignment" => {
                        let number = take_path(&mut args, "number");
                        (Method::PUT, format!("{plans}/draft/alignments/{number}"))
                    }
                    "answer_concern" => {
                        let id = take_path(&mut args, "concern_id");
                        (Method::PUT, format!("{plans}/draft/answers/{id}"))
                    }
                    "check_plan" => (Method::GET, format!("{plans}/draft/check")),
                    "submit_plan" => (Method::POST, format!("{plans}/draft/submission")),
                    "review_plan" => {
                        let revision = take_path(&mut args, "revision");
                        (Method::POST, format!("{plans}/{revision}/review"))
                    }
                    _ => (Method::GET, format!("{base}/tracks/{track}/units")),
                }
            }
            "get_unit_plan" | "get_unit_history" => {
                let number = take_path(&mut args, "number");
                let suffix = if name == "get_unit_history" {
                    "/history"
                } else {
                    "/plan"
                };
                (Method::GET, format!("{base}/units/{number}{suffix}"))
            }
            "raise_concern" => {
                let track = take_path(&mut args, "track");
                (Method::POST, format!("{base}/tracks/{track}/concerns"))
            }
            "list_concerns" => (Method::GET, format!("{base}/concerns")),
            "get_concern" | "dismiss_concern" => {
                let id = take_path(&mut args, "concern_id");
                if name == "dismiss_concern" {
                    (Method::POST, format!("{base}/concerns/{id}/dismissal"))
                } else {
                    (Method::GET, format!("{base}/concerns/{id}"))
                }
            }
            "ask" | "get_steering" | "post_steering" | "append_transcript" | "get_transcript" => {
                let number = take_path(&mut args, "number");
                let sequence = take_path(&mut args, "sequence");
                let (method, suffix) = match name {
                    "ask" => (Method::POST, "/questions"),
                    "get_steering" => (Method::GET, "/steering"),
                    "post_steering" => (Method::POST, "/steering"),
                    "append_transcript" => (Method::POST, "/transcript"),
                    _ => (Method::GET, "/transcript"),
                };
                (
                    method,
                    format!("{base}/units/{number}/attempts/{sequence}{suffix}"),
                )
            }
            "ask_job" => {
                let id = take_path(&mut args, "job_id");
                (Method::POST, format!("{base}/jobs/{id}/questions"))
            }
            "list_questions" => (Method::GET, format!("{base}/questions")),
            "get_question" | "wait_for_answer" | "answer_question" | "escalate_question" => {
                let id = take_path(&mut args, "question_id");
                let (method, suffix) = match name {
                    "wait_for_answer" => (Method::GET, "/answer"),
                    "answer_question" => (Method::POST, "/answer"),
                    "escalate_question" => (Method::POST, "/escalation"),
                    _ => (Method::GET, ""),
                };
                (method, format!("{base}/questions/{id}{suffix}"))
            }
            "ack_steering" => (Method::POST, format!("{base}/messages/acknowledgements")),
            "list_messages" => {
                let number = take_path(&mut args, "number");
                (Method::GET, format!("{base}/units/{number}/messages"))
            }
            "list_units" => (Method::GET, format!("{base}/units")),
            "get_unit" | "list_unit_revisions" => {
                let number = take_path(&mut args, "number");
                let suffix = if name == "list_unit_revisions" {
                    "/revisions"
                } else {
                    ""
                };
                (Method::GET, format!("{base}/units/{number}{suffix}"))
            }
            "claim_unit" => (Method::POST, format!("{base}/claims")),
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
                    format!("{base}/units/{number}/attempts/{sequence}{suffix}"),
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
                    // A decision case takes the document, a failure case the
                    // action: the form left out may arrive as nulls.
                    args.retain(|_, value| !value.is_null());
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
            "list_writeups" => (Method::GET, format!("{base}/writeups")),
            "get_writeup" | "write_up" | "skip_writeup" => {
                let number = take_path(&mut args, "number");
                let (method, suffix) = match name {
                    "write_up" => (Method::POST, ""),
                    "skip_writeup" => (Method::POST, "/skip"),
                    _ => (Method::GET, ""),
                };
                (method, format!("{base}/units/{number}/writeup{suffix}"))
            }
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
                    format!("{base}/units/{number}{sequence}/comments"),
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
                "create_track" | "transition_track" | "post_manifest" | "complete_job" | "fail_job"
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
    if values.len() != 77 {
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
