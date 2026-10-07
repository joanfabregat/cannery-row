use super::{
    identity::{Context, Session},
    lifecycle::{World, string},
};
use conformance::{Harness, Result};
use reqwest::Method;
use serde_json::{Value, json};

pub async fn admin(context: &mut Context) -> Result<Session> {
    context.login(json!({"sub":"conformance-admin","email":"admin@conformance.test","email_verified":true,"name":"Conformance admin"}),"/").await
}
pub async fn service_token(
    context: &mut Context,
    session: &Session,
    project: &str,
    service: &str,
    name: &str,
) -> Result<String> {
    let document = context
        .browser_api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("/api/projects/{project}/service-accounts/{service}/tokens"),
            session,
            Some(json!({"name":name,"expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    string(&document["token"])
}
pub async fn rpc(
    h: &Harness,
    token: &str,
    name: &str,
    arguments: Value,
    ua: Option<&str>,
) -> Result<Value> {
    let mut request=h.request(Method::POST,"/mcp")?.bearer_auth(token).header("Accept","application/json, text/event-stream").header("MCP-Protocol-Version","2025-06-18").header("X-Actor-User","fabricated-user").header("X-Via-Client","token:forged-admin").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}));
    if let Some(ua) = ua {
        request = request.header("user-agent", ua);
    }
    let response = request.send().await?;
    if response.status() != 200 {
        return Err(format!("MCP tool HTTP status {}", response.status()).into());
    }
    let body: Value = response.json().await?;
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert!(body.get("error").is_none());
    let result = body.get("result").ok_or("RPC result missing")?.clone();
    let content = result["content"].as_array().ok_or("MCP content missing")?;
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    let text: Value = serde_json::from_str(content[0]["text"].as_str().ok_or("MCP text missing")?)?;
    if text != result["structuredContent"] {
        return Err("MCP structured/text values differ".into());
    }
    Ok(result)
}
pub fn success(result: &Value, replayed: bool) -> Result<Value> {
    if result["isError"] != false {
        return Err("expected successful MCP tool result".into());
    }
    if replayed {
        assert_eq!(result["_meta"], json!({"replayed":true}));
        assert_eq!(result["structuredContent"]["replayed"], true);
    } else {
        assert!(result.get("_meta").is_none());
        assert!(result["structuredContent"].get("replayed").is_none());
    }
    Ok(result["structuredContent"].clone())
}
pub fn replay_body(first: &Value, replay: &Value) -> Result<()> {
    let mut expected = success(first, false)?;
    expected
        .as_object_mut()
        .ok_or("expected object result")?
        .insert("replayed".into(), json!(true));
    assert!(
        success(replay, true)? == expected,
        "replay domain changed apart from replay marker"
    );
    Ok(())
}
pub fn error(result: &Value, code: &str) {
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["code"], code);
    assert!(result.get("_meta").is_none());
    assert!(result["structuredContent"].get("replayed").is_none());
}
pub async fn audit(world: &mut World, token: &str, after: u64) -> Result<Value> {
    Ok(world.h.fetch_audit(token, after).await?.body["items"].clone())
}
pub fn rows<'a>(items: &'a Value, action: &str, subject: &Value) -> Result<Vec<&'a Value>> {
    Ok(items
        .as_array()
        .ok_or("audit items missing")?
        .iter()
        .filter(|row| row["action"] == action && row["subject_id"] == *subject)
        .collect())
}
pub fn actor(row: &Value, kind: &str, id: &Value, via: &str, client: &str) {
    assert_eq!(row["actor_kind"], kind);
    assert_eq!(
        row["actor_user_id"],
        if kind == "user" {
            id.clone()
        } else {
            Value::Null
        }
    );
    assert_eq!(
        row["actor_service_id"],
        if kind == "service" {
            id.clone()
        } else {
            Value::Null
        }
    );
    assert_eq!(row["via_channel"], via);
    assert_eq!(row["via_client"], client);
}
