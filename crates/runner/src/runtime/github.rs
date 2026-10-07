//! Credential-isolated GitHub acquisition and repository reachability checks.
use super::RuntimeError;
use crate::{
    code_store::{CodeCancellation, CodeError, CodeFuture, GitHubSource},
    credentials,
};
use cannery_core::principal::Secret;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use reqwest::{Client, Response, Url};
use serde_json::Value;
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;
pub mod app;

// Bound API work while covering repositories with more than one branch page.
// Requests use our API base and explicit page numbers, never a response Link URL.
const BRANCH_PAGE_SIZE: usize = 100;
const MAX_BRANCH_PAGES: usize = 100;

/// Credential selection is explicit; an App never degrades into anonymous access.
pub enum Credentials {
    Anonymous,
    File(PathBuf),
    App(std::sync::Arc<app::AppCredentials>),
}

/// A file credential is reread after a 401; it is never sent to a tarball redirect.
/// App installation signing is an additional adapter, not an anonymous fallback.
pub struct GitHub {
    client: Client,
    bucket: Client,
    api: Url,
    credentials: Credentials,
}
impl GitHub {
    /// # Errors
    /// Reject unsafe API bases and unusable configured credentials before requests.
    pub fn new(api: &str, token_file: Option<PathBuf>) -> Result<Self, RuntimeError> {
        Self::with_credentials(
            api,
            token_file.map_or(Credentials::Anonymous, Credentials::File),
        )
    }
    /// # Errors
    /// Reject unsafe API bases and unusable credentials before claims.
    pub fn from_settings(settings: &crate::config::GitHubSettings) -> Result<Self, RuntimeError> {
        let (api, credentials) = Self::checked_settings(settings)?;
        Self::with_credentials(&api, credentials)
    }
    pub(crate) fn check_settings(
        settings: &crate::config::GitHubSettings,
    ) -> Result<(), RuntimeError> {
        let (_, credentials) = Self::checked_settings(settings)?;
        if let Credentials::File(path) = credentials {
            crate::credentials::read_token_file(&path).map_err(|_| RuntimeError::Configuration)?;
        }
        Ok(())
    }
    fn checked_settings(
        settings: &crate::config::GitHubSettings,
    ) -> Result<(String, Credentials), RuntimeError> {
        let api = settings
            .api_url
            .as_ref()
            .map(|value| value.as_utf8().ok_or(RuntimeError::Configuration))
            .transpose()?
            .unwrap_or_else(|| "https://api.github.com".to_owned());
        let credentials = match (
            &settings.token_file,
            &settings.app_id,
            &settings.app_key_file,
            &settings.app_installation_id,
        ) {
            (None, None, None, None) => Credentials::Anonymous,
            (Some(path), None, None, None) => Credentials::File(path.clone()),
            (None, Some(id), Some(path), Some(installation)) => {
                Credentials::App(std::sync::Arc::new(app::AppCredentials::new(
                    &id.as_utf8().ok_or(RuntimeError::Configuration)?,
                    &installation.as_utf8().ok_or(RuntimeError::Configuration)?,
                    path,
                )?))
            }
            _ => return Err(RuntimeError::Configuration),
        };
        Self::check_api(&api)?;
        Ok((api, credentials))
    }
    /// # Errors
    /// Reject invalid bases and configured file credentials.
    pub fn with_credentials(api: &str, credentials: Credentials) -> Result<Self, RuntimeError> {
        let api = Self::check_api(api)?;
        if let Credentials::File(path) = &credentials {
            credentials::read_token_file(path).map_err(|_| RuntimeError::Configuration)?;
        }
        let build = || {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(60))
                .read_timeout(Duration::from_secs(300))
                .timeout(Duration::from_secs(300))
                .build()
                .map_err(|_| RuntimeError::Transport)
        };
        Ok(Self {
            client: build()?,
            bucket: build()?,
            api,
            credentials,
        })
    }
    fn check_api(api: &str) -> Result<Url, RuntimeError> {
        let api = Url::parse(api).map_err(|_| RuntimeError::Configuration)?;
        if !matches!(api.scheme(), "http" | "https")
            || api.host_str().is_none()
            || !api.username().is_empty()
            || api.password().is_some()
            || api.query().is_some()
            || api.fragment().is_some()
        {
            return Err(RuntimeError::Configuration);
        }
        Ok(api)
    }
    fn url(&self, repo: &str, suffix: &[&str]) -> Result<Url, CodeError> {
        let parts: Vec<_> = repo.split('/').collect();
        if parts.len() != 2
            || parts
                .iter()
                .any(|p| p.is_empty() || p.contains(['?', '#', '%', '\\']))
        {
            return Err(CodeError::InvalidCode);
        }
        let mut url = self.api.clone();
        {
            let mut path = url.path_segments_mut().map_err(|()| CodeError::GitHub)?;
            path.pop_if_empty()
                .push("repos")
                .push(parts[0])
                .push(parts[1]);
            for part in suffix {
                path.push(part);
            }
        }
        Ok(url)
    }
    async fn get(&self, url: Url, cancel: &CodeCancellation) -> Result<Response, CodeError> {
        let mut refreshed = false;
        let mut waited = false;
        loop {
            if cancel.requested() {
                return Err(CodeError::Cancelled);
            }
            let token: Option<Secret> = match &self.credentials {
                Credentials::File(path) => {
                    let path = path.clone();
                    Some(
                        tokio::task::spawn_blocking(move || credentials::read_token_file(&path))
                            .await
                            .map_err(|_| CodeError::GitHub)?
                            .map_err(|_| CodeError::GitHub)?,
                    )
                }
                Credentials::Anonymous => None,
                Credentials::App(app) => Some(app.token(&self.client, &self.api, cancel).await?),
            };
            let mut request = self
                .client
                .get(url.clone())
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "cannery-runner");
            if let Some(token) = &token {
                request = request.bearer_auth(token.expose());
            }
            let response = tokio::select! {
                ()=cancel.cancelled()=>return Err(CodeError::Cancelled),
                result=request.send()=>result.map_err(|_|CodeError::GitHub)?,
            };
            if response.status().as_u16() == 401 && token.is_some() && !refreshed {
                refreshed = true;
                if let Credentials::App(app) = &self.credentials {
                    app.invalidate(cancel).await?;
                }
                continue;
            }
            if let Some(wait) = rate_wait(&response) {
                if waited || wait > 60.0 {
                    return Err(CodeError::GitHub);
                }
                waited = true;
                tokio::select! {
                    ()=cancel.cancelled()=>return Err(CodeError::Cancelled),
                    ()=tokio::time::sleep(Duration::from_secs_f64(wait))=>{},
                }
                continue;
            }
            return Ok(response);
        }
    }
    async fn json(&self, url: Url, cancel: &CodeCancellation) -> Result<Value, CodeError> {
        let response = self.get(url, cancel).await?;
        if response.status().as_u16() != 200 {
            return Err(CodeError::GitHub);
        }
        tokio::select! {
            ()=cancel.cancelled()=>Err(CodeError::Cancelled),
            result=super::http::read_json(response)=>result.map_err(|_|CodeError::GitHub),
        }
    }
    async fn reachable(
        &self,
        repo: &str,
        base: &str,
        commit: &str,
        cancel: &CodeCancellation,
    ) -> Result<bool, CodeError> {
        let mut url = self.url(repo, &["compare", &format!("{base}...{commit}")])?;
        url.query_pairs_mut().append_pair("per_page", "1");
        let compared = self.json(url, cancel).await?;
        Ok(matches!(
            compared["status"].as_str(),
            Some("identical" | "behind")
        ))
    }
}
impl GitHubSource for GitHub {
    fn verify_commit<'a>(
        &'a self,
        repo: &'a str,
        commit: &'a str,
        cancel: CodeCancellation,
    ) -> CodeFuture<'a, ()> {
        Box::pin(async move {
            let about = self.json(self.url(repo, &[])?, &cancel).await?;
            let default = about["default_branch"].as_str().unwrap_or("");
            if !default.is_empty() && self.reachable(repo, default, commit, &cancel).await? {
                return Ok(());
            }
            for page in 1..=MAX_BRANCH_PAGES {
                let mut url = self.url(repo, &["branches"])?;
                url.query_pairs_mut()
                    .append_pair("per_page", "100")
                    .append_pair("page", &page.to_string());
                let branches = self.json(url, &cancel).await?;
                let branches = branches.as_array().ok_or(CodeError::GitHub)?;
                if branches.len() > BRANCH_PAGE_SIZE {
                    return Err(CodeError::GitHub);
                }
                for branch in branches {
                    if branch["name"] == default {
                        continue;
                    }
                    if let Some(sha) = branch["commit"]["sha"].as_str()
                        && !sha.is_empty()
                        && self.reachable(repo, sha, commit, &cancel).await?
                    {
                        return Ok(());
                    }
                }
                if branches.len() < BRANCH_PAGE_SIZE {
                    break;
                }
            }
            Err(CodeError::GitHub)
        })
    }
    fn download_tarball<'a>(
        &'a self,
        repo: &'a str,
        commit: &'a str,
        path: PathBuf,
        max_bytes: BigInt,
        cancel: CodeCancellation,
    ) -> CodeFuture<'a, ()> {
        Box::pin(async move {
            let mut response = self
                .get(self.url(repo, &["tarball", commit])?, &cancel)
                .await?;
            // Only the first API request carries credentials. Every subsequent
            // hop uses the cookie-free bucket client and validates its target.
            let mut redirects = 0;
            while response.status().is_redirection() {
                if redirects == 10 {
                    return Err(CodeError::GitHub);
                }
                redirects += 1;
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(CodeError::GitHub)?;
                let url = response
                    .url()
                    .join(location)
                    .map_err(|_| CodeError::GitHub)?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || (response.url().scheme() == "https" && url.scheme() != "https")
                {
                    return Err(CodeError::GitHub);
                }
                response = tokio::select! {
                    () = cancel.cancelled() => return Err(CodeError::Cancelled),
                    response = self.bucket.get(url).send() => response.map_err(|_| CodeError::GitHub)?,
                };
            }
            if response.status().as_u16() != 200 {
                return Err(CodeError::GitHub);
            }
            let mut output = tokio::fs::File::create(path)
                .await
                .map_err(|_| CodeError::GitHub)?;
            let mut written = BigInt::from(0);
            loop {
                let chunk = tokio::select! {
                    ()=cancel.cancelled()=>return Err(CodeError::Cancelled),
                    result=response.chunk()=>result.map_err(|_|CodeError::GitHub)?,
                };
                let Some(chunk) = chunk else {
                    break;
                };
                if cancel.requested() {
                    return Err(CodeError::Cancelled);
                }
                written += chunk.len();
                if written > max_bytes {
                    return Err(CodeError::GitHub);
                }
                output
                    .write_all(&chunk)
                    .await
                    .map_err(|_| CodeError::GitHub)?;
            }
            output.flush().await.map_err(|_| CodeError::GitHub)
        })
    }
}
fn rate_wait(response: &Response) -> Option<f64> {
    if !matches!(response.status().as_u16(), 403 | 429) {
        return None;
    }
    if let Some(seconds) = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Some(seconds.to_f64().unwrap_or(f64::INFINITY));
    }
    if response
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v == "0")
        && let Some(reset) = response
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    {
        return Some(
            reset.to_f64()?
                - SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_secs_f64(),
        )
        .map(|v| v.max(0.0) + 1.0);
    }
    (response.status().as_u16() == 429).then_some(60.0)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
