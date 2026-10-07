use super::identity::{Context, Session, opaque, string};
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

pub fn envelope_path() -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_CREDENTIAL_ENVELOPE")?);
    let parent = path.parent().ok_or("private envelope parent absent")?;
    if !path.is_absolute()
        || path.file_name().and_then(|n| n.to_str()) != Some("credentials.json")
        || parent.parent() != Some(std::path::Path::new("/tmp"))
        || !parent
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("conformance-credentials-"))
    {
        return Err("unexpected private RAM envelope path".into());
    }
    if fs::symlink_metadata(parent)?.file_type().is_symlink()
        || fs::metadata(parent)?.permissions().mode() & 0o777 != 0o700
    {
        return Err("private RAM directory permissions differ".into());
    }
    Ok(path)
}
pub fn write_envelope(value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(envelope_path()?)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    Ok(())
}
pub fn read_envelope() -> Result<Value> {
    let path = envelope_path()?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > 16384
    {
        return Err("private envelope file permissions or size differ".into());
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    if value["version"] != 1 {
        return Err("private envelope version differs".into());
    }
    for (kind, prefix) in [
        ("session", "cr_ses_"),
        ("pat", "cr_pat_"),
        ("service", "cr_svc_"),
        ("lease", "cr_lease_"),
        ("job", "cr_job_"),
        ("upload", "cr_upl_"),
        ("control", "cr_pat_"),
    ] {
        opaque(&string(&value[kind]["secret"])?, prefix)?;
    }
    Ok(value)
}
pub async fn me(ctx: &mut Context, token: &str, status: u16) -> Result<Value> {
    ctx.api(Method::GET, "/api/me", "/api/me", token, None, status)
        .await
}
pub async fn session_me(ctx: &mut Context, data: &Value, status: u16) -> Result<Value> {
    let request = ctx.h.request(Method::GET, "/api/me")?.header(
        "cookie",
        format!("cr_session={}", string(&data["session"]["secret"])?),
    );
    ctx.checked(Method::GET, "/api/me", request, status).await
}
pub async fn service_mint(
    ctx: &mut Context,
    session: &Session,
    project: &str,
) -> Result<(Value, Value)> {
    let base = format!("/api/projects/{project}");
    let account = ctx
        .browser_api(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &format!("{base}/service-accounts"),
            session,
            Some(json!({"kind":"agent","name":"credential-agent"})),
            201,
        )
        .await?;
    let token=ctx.browser_api(Method::POST,"/api/projects/{slug}/service-accounts/{name}/tokens",&format!("{base}/service-accounts/credential-agent/tokens"),session,Some(json!({"name":"credential-storage-svc","scopes":["read","write"],"expires_in_days":1})),201).await?;
    Ok((account, token))
}
