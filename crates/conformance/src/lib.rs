#![forbid(unsafe_code)]
//! Black-box checks: coverage is recorded only after a response passes its checks.
pub mod oidc;
pub mod requirements;

use reqwest::{Client, Method, Response, Url, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    time::Duration,
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub const OPENAPI: &str = include_str!("../../../web/openapi.json");

/// The launcher's independently generated contract for the implementation under test.
/// # Errors
/// Rejects missing or malformed configured contract documents.
pub fn contract_document() -> Result<Value> {
    match std::env::var("CANNERY_CONFORMANCE_OPENAPI_FILE") {
        Ok(path) => Ok(serde_json::from_slice(&std::fs::read(path)?)?),
        Err(std::env::VarError::NotPresent) => Ok(serde_json::from_str(OPENAPI)?),
        Err(error) => Err(error.into()),
    }
}

// These source handlers return opaque objects despite FastAPI's empty JSON schema.
const ARTIFACT_DOWNLOAD: &str = "/api/projects/{slug}/artifacts/{artifact_id}";
const RAW_OPERATIONS: [&str; 3] = [
    ARTIFACT_DOWNLOAD,
    "/api/projects/{slug}/jobs/{job_id}/inputs/object",
    "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/inputs/predecessor/{artifact_id}",
];

fn schema_status(method: &Method, template: &str, status: u16) -> u16 {
    // FastAPI documents these handlers' creation status; idempotent replays
    // return the same response model with status 200.
    if status == 200
        && method == Method::POST
        && [
            "/api/projects/{slug}/hypotheses",
            "/api/projects/{slug}/jobs/claims",
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
            "/api/projects/{slug}/review-cases/{case_id}/decisions",
        ]
        .contains(&template)
    {
        201
    } else {
        status
    }
}

fn check_raw_response(
    template: &str,
    status: u16,
    headers: &HeaderMap,
    bytes: &[u8],
) -> Result<()> {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if template == ARTIFACT_DOWNLOAD {
        let etag = header("etag").ok_or("artifact ETag missing")?;
        if etag.len() != 66
            || !etag.starts_with('"')
            || !etag.ends_with('"')
            || !etag[1..65].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("artifact ETag is not a quoted SHA-256".into());
        }
    }
    if status == 302 {
        let url = Url::parse(header("location").ok_or("download redirect Location missing")?)?;
        if !matches!(url.scheme(), "http" | "https")
            || header("cache-control") != Some("no-store")
            || !bytes.is_empty()
        {
            return Err("download redirect contract mismatch".into());
        }
        return Ok(());
    }
    if status == 304 && template != ARTIFACT_DOWNLOAD {
        return Err("304 only supported by artifact download".into());
    }
    if template == ARTIFACT_DOWNLOAD
        && (header("x-content-type-options") != Some("nosniff")
            || header("content-security-policy") != Some("default-src 'none'; sandbox")
            || header("cache-control") != Some("private, no-cache"))
    {
        return Err("artifact security/cache headers missing".into());
    }
    if status == 304 {
        if !bytes.is_empty() {
            return Err("304 response must be empty".into());
        }
        return Ok(());
    }
    let media = header("content-type").ok_or("download Content-Type missing")?;
    let mime = media
        .split(';')
        .next()
        .ok_or("invalid download media type")?;
    if !mime.contains('/') || mime.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err("invalid download media type".into());
    }
    if template == ARTIFACT_DOWNLOAD
        && ![
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "image/avif",
            "text/plain",
            "text/csv",
            "application/json",
            "application/jsonl",
            "application/x-ndjson",
            "application/octet-stream",
        ]
        .contains(&mime)
    {
        return Err("artifact has an unsafe served media type".into());
    }
    let size: usize = header("content-length")
        .ok_or("download Content-Length missing")?
        .parse()?;
    if size != bytes.len() {
        return Err("download size differs from Content-Length".into());
    }
    Ok(())
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct Coverage {
    pub operations: BTreeMap<String, BTreeSet<u16>>,
    pub tools: BTreeSet<String>,
    pub audit_actions: BTreeSet<String>,
}

impl Coverage {
    /// Requirements are supplied from the source inventory, never inferred from passed tests.
    #[must_use]
    pub fn missing(&self, requirements: &Self) -> Vec<String> {
        let mut missing = Vec::new();
        for (operation, statuses) in &requirements.operations {
            for status in statuses {
                if !self
                    .operations
                    .get(operation)
                    .is_some_and(|seen| seen.contains(status))
                {
                    missing.push(format!("{operation}: {status}"));
                }
            }
        }
        for tool in requirements.tools.difference(&self.tools) {
            missing.push(format!("tool: {tool}"));
        }
        for action in requirements.audit_actions.difference(&self.audit_actions) {
            missing.push(format!("audit: {action}"));
        }
        missing
    }

    pub fn merge(&mut self, other: &Self) {
        for (operation, statuses) in &other.operations {
            self.operations
                .entry(operation.clone())
                .or_default()
                .extend(statuses);
        }
        self.tools.extend(other.tools.iter().cloned());
        self.audit_actions
            .extend(other.audit_actions.iter().cloned());
    }
}

pub struct Harness {
    pub client: Client,
    base_url: Url,
    contract: Value,
    coverage: Coverage,
}

pub struct CheckedResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Value,
    /// Original bytes, including opaque download bodies. No implicit decoding occurs.
    pub raw_body: Vec<u8>,
}

impl Harness {
    /// Create an isolated cookie session for a configured HTTP server.
    ///
    /// # Errors
    /// Returns an error for invalid URLs, client configuration or frozen JSON.
    pub fn new(base_url: &str) -> Result<Self> {
        let base_url = Url::parse(base_url)?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err("expected an HTTP(S) base URL".into());
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .cookie_store(true)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base_url,
            contract: contract_document()?,
            coverage: Coverage::default(),
        })
    }

    /// Build a request confined to the configured server origin.
    ///
    /// # Errors
    /// Returns an error if the path is invalid or escapes the server origin.
    pub fn request(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let url = self.base_url.join(path)?;
        if url.origin() != self.base_url.origin() {
            return Err("request escaped configured server origin".into());
        }
        Ok(self.client.request(method, url))
    }

    #[must_use]
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Adds frozen-document success and documented error statuses. Auth and domain errors
    /// omitted from `FastAPI`'s document must be added by the source inventory.
    ///
    /// # Errors
    /// Returns an error if the frozen document's operation structure is invalid.
    pub fn documented_requirements(&self) -> Result<Coverage> {
        let mut requirements = Coverage::default();
        for (path, operations) in self.contract["paths"].as_object().ok_or("missing paths")? {
            for (method, operation) in operations.as_object().ok_or("invalid path item")? {
                if !matches!(
                    method.as_str(),
                    "get" | "post" | "patch" | "put" | "delete" | "head" | "options"
                ) {
                    continue;
                }
                let mut statuses = BTreeSet::new();
                for status in operation["responses"]
                    .as_object()
                    .ok_or("missing responses")?
                    .keys()
                {
                    if let Ok(status) = status.parse::<u16>()
                        && ((200..300).contains(&status)
                            || [401, 403, 404, 409, 422].contains(&status))
                    {
                        statuses.insert(status);
                    }
                }
                requirements
                    .operations
                    .insert(format!("{} {path}", method.to_uppercase()), statuses);
            }
        }
        Ok(requirements)
    }

    /// Transport success alone does not grant coverage. Status, JSON and response schema
    /// checks all pass first. Undocumented error bodies must match the shared API shape.
    ///
    /// # Errors
    /// Returns an error for transport, status, JSON, contract or schema failures.
    pub async fn check_response(
        &mut self,
        method: Method,
        template: &str,
        response: Response,
        expected: u16,
    ) -> Result<CheckedResponse> {
        let actual_segments: Vec<_> = response.url().path().split('/').collect();
        let template_segments: Vec<_> = template.split('/').collect();
        if response.url().origin() != self.base_url.origin()
            || actual_segments.len() != template_segments.len()
            || !actual_segments
                .iter()
                .zip(&template_segments)
                .all(|(actual, expected)| {
                    actual == expected
                        || (expected.starts_with('{')
                            && expected.ends_with('}')
                            && !actual.is_empty())
                })
        {
            return Err("response URL does not match claimed frozen operation".into());
        }
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;
        if status != expected {
            return Err(format!("{method} {template}: expected {expected}, got {status}").into());
        }
        let operation = &self.contract["paths"][template][method.as_str().to_lowercase()];
        if operation.is_null() {
            return Err(format!("unknown frozen operation {method} {template}").into());
        }
        let opaque = method == Method::GET
            && RAW_OPERATIONS.contains(&template)
            && [200, 302, 304].contains(&status);
        if status == 204 && !bytes.is_empty() {
            return Err("204 response must be empty".into());
        }
        let body = if opaque {
            check_raw_response(template, status, &headers, &bytes)?;
            Value::Null
        } else if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        let schema_status = schema_status(&method, template, status);
        if opaque {
            // These three handlers explicitly return Response/StreamingResponse; the
            // frozen FastAPI document supplies an empty JSON schema as a placeholder.
        } else if let Some(schema) =
            operation["responses"][schema_status.to_string()]["content"]["application/json"]["schema"]
                .as_object()
        {
            if !headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| s.starts_with("application/json"))
            {
                return Err("expected JSON content type".into());
            }
            let mut root = Value::Object(schema.clone());
            root["components"] = self.contract["components"].clone();
            let validator = jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .should_validate_formats(true)
                .offline()
                .build(&root)?;
            let errors: Vec<_> = validator
                .iter_errors(&body)
                .map(|e| e.instance_path().to_string())
                .collect();
            if !errors.is_empty() {
                return Err(format!("response schema violations: {}", errors.join("; ")).into());
            }
        } else if status < 400 && !bytes.is_empty() {
            return Err(format!("undocumented successful response {status}").into());
        }
        if status >= 400
            && (body["error"]["code"].as_str().is_none()
                || body["error"]["message"].as_str().is_none()
                || body["error"].get("details").is_none())
        {
            return Err("error did not match API error shape".into());
        }
        self.coverage
            .operations
            .entry(format!("{method} {template}"))
            .or_default()
            .insert(status);
        Ok(CheckedResponse {
            status,
            headers,
            body,
            raw_body: bytes.to_vec(),
        })
    }

    /// Send a stateless MCP JSON-RPC request without crediting coverage.
    ///
    /// # Errors
    /// Returns an error for URL construction or transport failures.
    pub async fn rpc(&self, token: Option<&str>, method: &str, params: Value) -> Result<Response> {
        let mut request = self
            .request(Method::POST, "/mcp")?
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .header("user-agent", "cannery-conformance/0.1")
            .json(&json!({"jsonrpc":"2.0", "id":1,"method":method,"params":params}));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        Ok(request.send().await?)
    }

    /// Records tools only when HTTP and JSON-RPC checks, text parity and success pass.
    ///
    /// # Errors
    /// Returns an error for transport, protocol, JSON or tool failures.
    pub async fn call_tool(&mut self, token: &str, name: &str, arguments: Value) -> Result<Value> {
        let response = self
            .rpc(
                Some(token),
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            )
            .await?;
        if response.status() != 200 {
            return Err(format!("MCP HTTP status {}", response.status()).into());
        }
        let raw_body = response.bytes().await?.to_vec();
        let body: Value = serde_json::from_slice(&raw_body)?;
        if body["jsonrpc"] != "2.0" || body["id"] != 1 || !body["error"].is_null() {
            return Err("invalid RPC result".into());
        }
        let result = &body["result"];
        let text: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .ok_or("missing MCP content")?,
        )?;
        if text != result["structuredContent"] || result["isError"] != false {
            return Err("MCP tool failed or textual/structured output differed".into());
        }
        self.coverage.tools.insert(name.to_owned());
        Ok(text)
    }

    /// Accept only audit rows returned by a checked audit endpoint response.
    ///
    /// # Errors
    /// Returns an error for missing items or non-string action fields.
    pub fn observe_audit(&mut self, checked: &CheckedResponse) -> Result<()> {
        let items = checked.body["items"]
            .as_array()
            .ok_or("audit items missing")?;
        let actions = items
            .iter()
            .map(|row| {
                row["action"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("audit action missing")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.coverage.audit_actions.extend(actions);
        Ok(())
    }

    /// The test-only audit hook is intentionally absent from the frozen public contract.
    /// Check its explicit wire shape and collect observed actions after the request passes.
    ///
    /// # Errors
    /// Returns an error for transport, status or audit response shape failures.
    pub async fn fetch_audit(&mut self, token: &str, after: u64) -> Result<CheckedResponse> {
        let response = self
            .request(Method::GET, "/__conformance/audit")?
            .bearer_auth(token)
            .query(&[("after", after)])
            .send()
            .await?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("test audit endpoint returned {status}").into());
        }
        let headers = response.headers().clone();
        let raw_body = response.bytes().await?.to_vec();
        let body: Value = serde_json::from_slice(&raw_body)?;
        if body["next_after"].as_u64().is_none() {
            return Err("audit cursor missing".into());
        }
        let checked = CheckedResponse {
            status,
            headers,
            body,
            raw_body,
        };
        self.observe_audit(&checked)?;
        Ok(checked)
    }

    /// Trigger real-time sweeps; this does not simulate or change the database clock.
    ///
    /// # Errors
    /// Returns an error for transport, status or JSON failures.
    pub async fn sweep(&self, token: &str) -> Result<Value> {
        let response = self
            .request(Method::POST, "/__conformance/sweep")?
            .bearer_auth(token)
            .send()
            .await?;
        if response.status() != 200 {
            return Err(format!("test sweep endpoint returned {}", response.status()).into());
        }
        Ok(response.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_reports_missing_and_unions_results() -> Result<()> {
        let harness = Harness::new("http://localhost:9000")?;
        let required = harness.documented_requirements()?;
        assert_ne!(harness.coverage().missing(&required), Vec::<String>::new());
        let mut merged = Coverage::default();
        merged.merge(&required);
        assert_eq!(merged.missing(&required), Vec::<String>::new());
        Ok(())
    }
    #[test]
    fn requests_cannot_escape_origin() -> Result<()> {
        let harness = Harness::new("http://localhost:9000")?;
        assert!(
            harness
                .request(Method::GET, "https://example.com/")
                .is_err()
        );
        assert!(harness.request(Method::GET, "//example.com/").is_err());
        Ok(())
    }
}
