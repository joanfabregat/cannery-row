#![forbid(unsafe_code)]
use conformance::{Harness, Result};
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .ok_or("usage: conformance BASE_URL REPORT_PATH [REQUIREMENTS_PATH]")?;
    let output = args.next().ok_or("coverage report path required")?;
    let requirements = args.next();
    let mut harness = Harness::new(&url)?;
    let health = harness.request(Method::GET, "/api/health")?.send().await?;
    let health = harness
        .check_response(Method::GET, "/api/health", health, 200)
        .await?;
    if health.body["database"] != "ok" {
        return Err("database health is not ok".into());
    }
    for path in ["/api/me", "/api/projects"] {
        let response = harness.request(Method::GET, path)?.send().await?;
        let checked = harness
            .check_response(Method::GET, path, response, 401)
            .await?;
        if checked.body["error"]["code"] != "unauthenticated" {
            return Err("wrong unauthenticated error code".into());
        }
    }
    for method in [Method::GET, Method::DELETE] {
        let response = harness.request(method, "/mcp")?.send().await?;
        if response.status() != 405
            || response
                .headers()
                .get("allow")
                .and_then(|v| v.to_str().ok())
                != Some("POST")
        {
            return Err("MCP stateless method rejection mismatch".into());
        }
        let body: Value = response.json().await?;
        if body["error"]["code"] != "method_not_allowed" {
            return Err("wrong MCP method rejection code".into());
        }
    }
    let response = harness.rpc(None, "ping", json!({})).await?;
    if response.status() != 401
        || response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            != Some("Bearer")
    {
        return Err("MCP unauthenticated rejection mismatch".into());
    }
    let body: Value = response.json().await?;
    if body["error"]["code"] != "unauthenticated" {
        return Err("wrong MCP authentication code".into());
    }
    let response = harness
        .request(Method::POST, "/mcp")?
        .header("MCP-Protocol-Version", "2025-06-18")
        .body("{")
        .send()
        .await?;
    if response.status() != 400 {
        return Err("malformed MCP JSON was not rejected before authentication".into());
    }
    let body: Value = response.json().await?;
    if body["error"]["code"] != -32700 || !body["id"].is_null() {
        return Err("wrong MCP JSON parse error".into());
    }
    std::fs::write(output, serde_json::to_vec_pretty(harness.coverage())?)?;
    if let Some(path) = requirements {
        let requirements = serde_json::from_slice(&std::fs::read(path)?)?;
        let missing = harness.coverage().missing(&requirements);
        if !missing.is_empty() {
            return Err(format!("missing conformance coverage:\n{}", missing.join("\n")).into());
        }
    }
    Ok(())
}
