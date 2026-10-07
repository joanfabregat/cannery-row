#![forbid(unsafe_code)]
use conformance::Result;
use conformance::oidc::{JwksMode, LogoutMode};
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let bind: std::net::SocketAddr = args
        .next()
        .ok_or("usage: oidc-stub LOOPBACK:PORT ISSUER")?
        .parse()?;
    if !bind.ip().is_loopback() {
        return Err("OIDC test stub must bind to loopback".into());
    }
    let issuer = args.next().ok_or("issuer required")?;
    let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
    let jwks = match std::env::var("CANNERY_TEST_OIDC_JWKS_MODE")
        .as_deref()
        .unwrap_or("plain")
    {
        "plain" => JwksMode::Plain,
        "decoys" => JwksMode::Decoys,
        "unusable" => JwksMode::Unusable,
        _ => return Err("unknown test OIDC JWKS mode".into()),
    };
    let logout = match std::env::var("CANNERY_TEST_OIDC_LOGOUT_MODE")
        .as_deref()
        .unwrap_or("default")
    {
        "default" => LogoutMode::Default,
        "none" => LogoutMode::None,
        "remote-http" => LogoutMode::RemoteHttp,
        "javascript" => LogoutMode::Javascript,
        "hostless-https" => LogoutMode::HostlessHttps,
        "localhost-http" => LogoutMode::LocalhostHttp,
        "remote-https" => LogoutMode::RemoteHttps,
        _ => return Err("unknown test OIDC logout mode".into()),
    };
    let router = conformance::oidc::router_with_modes(&issuer, &control, jwks, logout)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
