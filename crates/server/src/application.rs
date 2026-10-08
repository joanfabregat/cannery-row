//! Application startup keeps migration separate from serving.

use crate::{
    documentation,
    health::{HealthState, health},
    requests::{RequestState, contextualize},
    web::{WebError, WebState},
};
use axum::{
    Json, Router,
    http::{StatusCode, header},
    middleware,
    response::IntoResponse,
    routing::get,
};
use cannery_core::{
    db::{ConnectionError, DatabaseOptions},
    settings::{Settings, SettingsError},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{sync::Arc, time::Duration};

#[path = "production_contexts.rs"]
mod production_contexts;

/// Shared bounded validation policy for installation commands and HTTP writes.
#[must_use]
pub fn research_validation_policy() -> (cannery_research::science::RenderingContext, usize) {
    production_contexts::research_validation_policy()
}

#[derive(thiserror::Error, Debug)]
pub enum ServerError {
    #[error("invalid recovery stream limit")]
    RecoveryLimits,
    #[error("invalid output validation limits")]
    OutputValidationLimits,
    #[error(transparent)]
    Recovery(#[from] crate::sweeps::SweepError),
    #[error(transparent)]
    Mcp(#[from] crate::mcp::StartupError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Web(#[from] WebError),
    #[error("invalid database pool size")]
    PoolSize,
    #[error("conformance hooks are unavailable in the production package")]
    ProductionTesting,
    #[error("could not initialize document contracts")]
    Contracts,
    #[error("invalid OIDC configuration or unavailable HTTP client")]
    OidcConfiguration,
    #[error(transparent)]
    Storage(#[from] cannery_storage::Error),
    #[error("upload cancellation settlement failed")]
    Settlement,
    #[error("could not bind or serve the HTTP listener: {0}")]
    Listener(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    pub database: DatabaseOptions,
    pub pool: PgPool,
    pub oidc: Option<Arc<dyn crate::oidc_provider::OidcProvider>>,
    pub sweeps: Option<Arc<crate::sweeps::SweepContext>>,
}

/// Construct the HTTP application without applying migrations.
///
/// # Errors
/// Refuses invalid settings, pool sizes, or a missing web distribution.
pub fn application(settings: Settings) -> Result<(Router, AppState), ServerError> {
    application_with_oidc(settings, None)
}

/// Assemble implemented research routes and real configured OIDC for serving.
/// # Errors
/// Refuses invalid startup resources, storage configuration and contracts.
pub async fn production_application(
    settings: Settings,
    forwarded_allow_ips: &str,
) -> Result<
    (
        Router,
        AppState,
        Arc<crate::upload_routes::CancellationTasks>,
    ),
    ServerError,
> {
    let oidc = production_contexts::provider(&settings)?;
    let store = Arc::new(cannery_storage::create_store(&settings.storage).await?);
    let (contexts, cancellation) = production_contexts::contexts(&store, &settings)?;
    let (router, state) = application_with_proxy(settings, forwarded_allow_ips, oidc, contexts)?;
    Ok((router, state, cancellation))
}
/// Install attempt claims with explicit entry contexts, leaving defaults unbound.
/// # Errors
/// Preserves ordinary application startup failures.
pub fn application_with_attempt_claim_context(
    settings: Settings,
    context: Arc<crate::attempt_claim_routes::AttemptClaimContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            attempt_claims: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct the application with an explicitly supplied OIDC backend.
/// # Errors
/// Refuses the same invalid settings and startup resources as `application`.
pub fn application_with_oidc(
    settings: Settings,
    oidc: Option<Arc<dyn crate::oidc_provider::OidcProvider>>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(settings, "127.0.0.1,::1", oidc, RouteContexts::default())
}

/// Construct configuration routes with explicitly calibrated operation contexts.
/// # Errors
/// Preserves application startup errors; no profile or regex-cache policy is inferred.
pub fn application_with_config_context(
    settings: Settings,
    context: Arc<crate::config_routes::ConfigContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            config: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct track routes with explicit caller-selected operation contexts.
/// # Errors
/// Preserves startup failures without installing a default compatibility profile.
pub fn application_with_track_context(
    settings: Settings,
    context: Arc<crate::track_routes::TrackContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            tracks: Some(context),
            ..RouteContexts::default()
        },
    )
}

#[derive(Default)]
struct RouteContexts {
    job_uploads: Option<Arc<crate::job_upload_routes::JobUploadContext>>,
    job_lifecycle: Option<Arc<crate::job_completion_routes::JobLifecycleContext>>,
    sweeps: Option<Arc<crate::sweeps::SweepContext>>,
    submissions: Option<Arc<crate::submission_routes::SubmissionContext>>,
    steps: Option<Arc<crate::step_routes::StepContext>>,
    manifests: Option<Arc<crate::manifest_routes::ManifestContext>>,
    uploads: Option<Arc<crate::upload_routes::UploadContext>>,
    predecessor_input: Option<Arc<crate::predecessor_input_routes::PredecessorInputContext>>,
    job_inputs: Option<Arc<crate::job_input_routes::JobInputContext>>,
    job_reads: Option<Arc<crate::job_read_routes::JobReadContext>>,
    attempt_claims: Option<Arc<crate::attempt_claim_routes::AttemptClaimContext>>,
    job_claims: Option<Arc<crate::job_claim_routes::JobClaimContext>>,
    download: Option<Arc<crate::artifact_download::DownloadContext>>,
    attempt_leases: Option<Arc<crate::attempt_lease_routes::AttemptLeaseContext>>,
    reports: Option<Arc<crate::report_routes::ReportContext>>,
    attempt_reads: Option<Arc<crate::attempt_read_routes::AttemptReadContext>>,
    config: Option<Arc<crate::config_routes::ConfigContext>>,
    tracks: Option<Arc<crate::track_routes::TrackContext>>,
    hypotheses: Option<Arc<crate::hypothesis_routes::HypothesisContext>>,
    review_attention: Option<Arc<crate::review_attention_routes::ReviewAttentionContext>>,
    metrics: Option<Arc<crate::metric_routes::MetricContext>>,
    search: Option<Arc<crate::search_routes::SearchContext>>,
    comments: Option<Arc<crate::comment_routes::CommentContext>>,
    review_decisions: Option<Arc<crate::review_decision_routes::ReviewDecisionContext>>,
}
#[allow(clippy::too_many_lines)] // Route assembly keeps optional installation boundaries visible.
fn application_with_proxy(
    settings: Settings,
    forwarded_allow_ips: &str,
    oidc: Option<Arc<dyn crate::oidc_provider::OidcProvider>>,
    contexts: RouteContexts,
) -> Result<(Router, AppState), ServerError> {
    if settings.testing.enabled && !cfg!(feature = "conformance-testing") {
        return Err(ServerError::ProductionTesting);
    }
    let minimum = settings
        .database
        .pool_min_size
        .to_i64("database.pool_min_size")?;
    let maximum = settings
        .database
        .pool_max_size
        .to_i64("database.pool_max_size")?;
    if minimum < 0 || maximum <= 0 || maximum < minimum {
        return Err(ServerError::PoolSize);
    }
    let minimum = u32::try_from(minimum).map_err(|_| ServerError::PoolSize)?;
    let maximum = u32::try_from(maximum).map_err(|_| ServerError::PoolSize)?;
    let database = DatabaseOptions::parse(&settings.database.url)?;
    let pool = PgPoolOptions::new()
        .min_connections(minimum)
        .max_connections(maximum)
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy_with(database.connect_options().clone());
    let web = WebState::new(settings.web.dist_dir.as_deref())?;
    let health_state = HealthState::new(pool.clone(), database.connect_options().clone());
    let state = AppState {
        settings: Arc::new(settings),
        database,
        pool,
        oidc,
        sweeps: contexts.sweeps.clone(),
    };
    let requests = RequestState::new(forwarded_allow_ips);
    let mcp_download = contexts.download.clone();
    let inner = Router::new()
        .route("/api/health", get(health).head(health_method_not_allowed))
        .with_state(health_state)
        .merge(documentation::routes())
        .merge(crate::schema_routes::routes()?)
        .merge(crate::identity_routes::routes(state.clone()))
        .merge(crate::browser_routes::routes(state.clone()))
        .merge(crate::project_routes::routes(state.clone()))
        .merge(crate::brief_routes::routes(state.clone())?)
        .merge(contexts.attempt_claims.map_or_else(Router::new, |context| {
            crate::attempt_claim_routes::routes(state.clone(), context)
        }))
        .merge(contexts.job_claims.map_or_else(Router::new, |context| {
            crate::job_claim_routes::routes(state.clone(), context)
        }))
        .merge(contexts.job_reads.map_or_else(Router::new, |context| {
            crate::job_read_routes::routes(state.clone(), context)
        }))
        .merge(contexts.job_inputs.map_or_else(Router::new, |context| {
            crate::job_input_routes::routes(state.clone(), context)
        }))
        .merge(contexts.job_uploads.map_or_else(Router::new, |context| {
            crate::job_upload_routes::routes(state.clone(), context)
        }))
        .merge(contexts.job_lifecycle.map_or_else(Router::new, |context| {
            crate::job_completion_routes::routes(state.clone(), context)
        }))
        .merge(
            contexts
                .predecessor_input
                .map_or_else(Router::new, |context| {
                    crate::predecessor_input_routes::routes(state.clone(), context)
                }),
        )
        .merge(contexts.config.map_or_else(Router::new, |context| {
            crate::config_routes::routes(state.clone(), context)
        }))
        .merge(contexts.tracks.map_or_else(Router::new, |context| {
            crate::track_routes::routes(state.clone(), context)
        }))
        .merge(
            contexts
                .review_attention
                .map_or_else(Router::new, |context| {
                    crate::review_attention_routes::routes(state.clone(), context)
                }),
        )
        .merge(contexts.hypotheses.map_or_else(Router::new, |context| {
            crate::hypothesis_routes::routes(state.clone(), context)
        }))
        .merge(contexts.metrics.map_or_else(Router::new, |context| {
            crate::metric_routes::routes(state.clone(), context)
        }))
        .merge(contexts.search.map_or_else(Router::new, |context| {
            crate::search_routes::routes(state.clone(), context)
        }))
        .merge(contexts.comments.map_or_else(Router::new, |context| {
            crate::comment_routes::routes(state.clone(), context)
        }))
        .merge(
            contexts
                .review_decisions
                .map_or_else(Router::new, |context| {
                    crate::review_decision_routes::routes(state.clone(), context)
                }),
        )
        .merge(contexts.download.map_or_else(Router::new, |context| {
            crate::artifact_download::routes(state.clone(), context)
        }))
        .merge(contexts.attempt_reads.map_or_else(Router::new, |context| {
            crate::attempt_read_routes::routes(state.clone(), context)
        }))
        .merge(contexts.attempt_leases.map_or_else(Router::new, |context| {
            crate::attempt_lease_routes::routes(state.clone(), context)
        }))
        .merge(contexts.reports.map_or_else(Router::new, |context| {
            crate::report_routes::routes(state.clone(), context)
        }))
        .merge(crate::comparison_routes::routes(state.clone()))
        .merge(contexts.submissions.map_or_else(Router::new, |context| {
            crate::submission_routes::routes(state.clone(), context)
        }))
        .merge(contexts.steps.map_or_else(Router::new, |context| {
            crate::step_routes::routes(state.clone(), context)
        }))
        .merge(contexts.manifests.map_or_else(Router::new, |context| {
            crate::manifest_routes::routes(state.clone(), context)
        }))
        .merge(web.router())
        .merge(contexts.uploads.map_or_else(Router::new, |context| {
            crate::upload_routes::routes(state.clone(), context)
        }))
        .method_not_allowed_fallback(method_not_allowed);
    let mcp = crate::mcp::routes(state.clone(), inner.clone(), mcp_download)?;
    let inner = inner.merge(mcp);
    #[cfg(feature = "conformance-testing")]
    let inner = if state.settings.testing.enabled {
        inner.merge(crate::conformance_hooks::routes(state.clone()))
    } else {
        inner
    };
    Ok((request_layers(inner, requests), state))
}

/// Install upload routes with caller-selected storage and operation contexts.
/// # Errors
/// Preserves application startup failures.
pub fn application_with_upload_context(
    settings: Settings,
    context: Arc<crate::upload_routes::UploadContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            uploads: Some(context),
            ..RouteContexts::default()
        },
    )
}

fn request_layers(inner: Router, requests: Arc<RequestState>) -> Router {
    // Path normalization must run before the inner router chooses a handler.
    Router::new()
        .fallback_service(inner)
        .layer(middleware::from_fn_with_state(
            crate::transport::TransportState {
                requests: requests.clone(),
                routes: crate::transport::installed_routes(),
            },
            crate::transport::normalize,
        ))
        .layer(middleware::from_fn_with_state(requests, contextualize))
}

/// Construct heartbeat routes with explicit entry-selected lease profiles.
/// # Errors
/// Preserves application startup failures without installing default profiles.
pub fn application_with_attempt_lease_context(
    settings: Settings,
    context: Arc<crate::attempt_lease_routes::AttemptLeaseContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            attempt_leases: Some(context),
            ..RouteContexts::default()
        },
    )
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}

async fn health_method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}

/// Serve until interrupted, then drain connections and close the pool.
///
/// # Errors
/// Reports startup configuration or HTTP listener failures.
pub async fn serve(
    settings: Settings,
    host: &str,
    port: u16,
    forwarded_allow_ips: &str,
) -> Result<(), ServerError> {
    let (app, state, cancellation) = production_application(settings, forwarded_allow_ips).await?;
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    let recovery = if state.settings.sweeps.enabled {
        Some(crate::sweeps::Task::start(state.clone())?)
    } else {
        None
    };
    tracing::info!(host, port, "HTTP listener ready");
    let outcome = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<crate::requests::ConnectionAddresses>(),
    )
    .with_graceful_shutdown(shutdown())
    .await;
    if let Some(recovery) = recovery {
        recovery.stop().await;
    }
    let settlement = cancellation.drain().await;
    state.pool.close().await;
    settlement.map_err(|_| ServerError::Settlement)?;
    outcome.map_err(ServerError::from)
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Construct five hypothesis routes with explicitly selected compatibility contexts.
/// # Errors
/// Preserves application startup failures without choosing a production profile.
pub fn application_with_hypothesis_context(
    settings: Settings,
    context: Arc<crate::hypothesis_routes::HypothesisContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            hypotheses: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct read routes with explicit caller-supplied compatibility contexts.
/// # Errors
/// Preserves startup failures without binding a production profile.
pub fn application_with_review_attention_context(
    settings: Settings,
    context: Arc<crate::review_attention_routes::ReviewAttentionContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            review_attention: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct metric routes with explicitly calibrated operation contexts.
/// # Errors
/// Preserves startup errors without inferring production model limits.
pub fn application_with_metric_context(
    settings: Settings,
    context: Arc<crate::metric_routes::MetricContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            metrics: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct decision routes with explicit source frontend/repository contexts.
/// # Errors
/// Preserves startup failures and leaves the default application unbound.
pub fn application_with_review_decision_context(
    settings: Settings,
    context: Arc<crate::review_decision_routes::ReviewDecisionContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            review_decisions: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct comment reads with explicit caller repository profiles.
/// # Errors
/// Preserves application startup errors without binding production histories.
pub fn application_with_comment_context(
    settings: Settings,
    context: Arc<crate::comment_routes::CommentContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            comments: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct search routes with an explicit source execution profile.
/// # Errors
/// Preserves startup errors without inferring production pool ownership.
pub fn application_with_search_context(
    settings: Settings,
    context: Arc<crate::search_routes::SearchContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            search: Some(context),
            ..RouteContexts::default()
        },
    )
}
/// Construct attempt reads with explicit profiles and bounded preparation factories.
/// # Errors
/// Preserves startup failures; ordinary application construction remains unbound.
pub fn application_with_attempt_read_context(
    settings: Settings,
    context: Arc<crate::attempt_read_routes::AttemptReadContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            attempt_reads: Some(context),
            ..RouteContexts::default()
        },
    )
}
/// Construct report JSON reads with explicit compatibility profiles.
/// # Errors
/// Preserves startup errors; default application remains unbound.
pub fn application_with_report_context(
    settings: Settings,
    context: Arc<crate::report_routes::ReportContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            reports: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct artifact delivery with explicit storage and compatibility profiles.
/// # Errors
/// Returns existing application startup errors.
pub fn application_with_download_context(
    settings: Settings,
    context: Arc<crate::artifact_download::DownloadContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            download: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Install job claims and heartbeat only with explicitly supplied compatibility profiles.
/// # Errors
/// Preserves startup failures; default production binding is unchanged.
pub fn application_with_job_claim_context(
    settings: Settings,
    context: Arc<crate::job_claim_routes::JobClaimContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_claims: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Construct the two ordinary job reads with explicit compatibility profiles.
/// # Errors
/// Returns configuration or database-pool setup failures.
pub fn application_with_job_read_context(
    settings: Settings,
    context: Arc<crate::job_read_routes::JobReadContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_reads: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Install ordinary reads together with existing claim/heartbeat method ownership.
/// # Errors
/// Returns configuration or database-pool setup failures.
pub fn application_with_job_read_and_claim_context(
    settings: Settings,
    reads: Arc<crate::job_read_routes::JobReadContext>,
    claims: Arc<crate::job_claim_routes::JobClaimContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_reads: Some(reads),
            job_claims: Some(claims),
            ..RouteContexts::default()
        },
    )
}

/// Explicit job input installation; all compatibility and store profiles are caller-owned.
/// # Errors
/// Returns the existing startup failures without changing default route installation.
pub fn application_with_job_input_context(
    settings: Settings,
    inputs: Arc<crate::job_input_routes::JobInputContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_inputs: Some(inputs),
            ..RouteContexts::default()
        },
    )
}

/// Explicit combined job HTTP installation with caller-owned compatibility profiles.
/// # Errors
/// Returns the existing startup failures.
pub fn application_with_job_contexts(
    settings: Settings,
    reads: Arc<crate::job_read_routes::JobReadContext>,
    claims: Arc<crate::job_claim_routes::JobClaimContext>,
    inputs: Arc<crate::job_input_routes::JobInputContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_reads: Some(reads),
            job_claims: Some(claims),
            job_inputs: Some(inputs),
            ..RouteContexts::default()
        },
    )
}

/// Explicit predecessor delivery installation, with no inferred production profiles.
/// # Errors
/// Returns existing startup failures.
pub fn application_with_predecessor_input_context(
    settings: Settings,
    context: Arc<crate::predecessor_input_routes::PredecessorInputContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            predecessor_input: Some(context),
            ..RouteContexts::default()
        },
    )
}
/// Install worker completion/failure with explicit operation profiles.
/// # Errors
/// Preserves application startup errors and leaves ordinary defaults unbound.
pub fn application_with_job_lifecycle_context(
    settings: Settings,
    context: Arc<crate::job_completion_routes::JobLifecycleContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_lifecycle: Some(context),
            ..RouteContexts::default()
        },
    )
}

/// Install the complete worker job lifecycle and its cancellation-safe upload receiver.
/// # Errors
/// Preserves application startup failures; every execution profile is caller supplied.
pub fn application_with_job_lifecycle_and_upload_context(
    settings: Settings,
    lifecycle: Arc<crate::job_completion_routes::JobLifecycleContext>,
    uploads: Arc<crate::job_upload_routes::JobUploadContext>,
) -> Result<(Router, AppState), ServerError> {
    application_with_proxy(
        settings,
        "127.0.0.1,::1",
        None,
        RouteContexts {
            job_lifecycle: Some(lifecycle),
            job_uploads: Some(uploads),
            ..RouteContexts::default()
        },
    )
}
