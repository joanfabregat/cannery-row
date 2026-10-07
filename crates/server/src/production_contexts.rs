//! Explicit production policies, separate from historical source fixture profiles.
use super::{RouteContexts, ServerError};
use crate::{
    oidc_client::{ClientConfig, OidcClient, SystemMonotonicClock},
    oidc_native::{HttpTransport, RingVerifier},
    oidc_protocol::RenderingContext,
    oidc_provider::OidcProvider,
};
use cannery_core::{contracts::ContractValidator, settings::Settings};
use cannery_storage::ObjectStore;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

/// A documented application limit rather than a Python interpreter frontier.
const DEPTH: usize = cannery_core::json::MAX_DEPTH;
pub(super) fn research_validation_policy() -> (cannery_research::science::RenderingContext, usize) {
    (science(), DEPTH)
}
fn validator() -> Result<ContractValidator, ServerError> {
    ContractValidator::new().map_err(|_| ServerError::Contracts)
}

fn science() -> cannery_research::science::RenderingContext {
    cannery_research::science::RenderingContext {
        nesting_budget: DEPTH,
    }
}
fn config() -> cannery_research::config_repo::JsonContext {
    cannery_research::config_repo::JsonContext {
        encode_nesting_budget: DEPTH,
        decode_nesting_budget: DEPTH,
    }
}
fn attempts() -> cannery_attempts::model::JsonContext {
    cannery_attempts::model::JsonContext {
        encode_nesting_budget: DEPTH,
        decode_nesting_budget: DEPTH,
    }
}
fn hypotheses() -> cannery_hypotheses::repo::JsonContext {
    cannery_hypotheses::repo::JsonContext {
        encode_nesting_budget: DEPTH,
        decode_nesting_budget: DEPTH,
    }
}
fn jobs() -> cannery_jobs::repo::JsonContext {
    cannery_jobs::repo::JsonContext {
        encode_nesting_budget: DEPTH,
        decode_nesting_budget: DEPTH,
    }
}
fn tracks() -> cannery_tracks::repo::JsonContext {
    cannery_tracks::repo::JsonContext {
        encode_nesting_budget: DEPTH,
        decode_nesting_budget: DEPTH,
    }
}

fn review_reads() -> crate::review_attention_routes::ReviewAttentionContext {
    crate::review_attention_routes::ReviewAttentionContext {
        reviews: cannery_reviews::JsonContext {
            decode_nesting_budget: DEPTH,
        },
        hypotheses: hypotheses(),
        attempts: attempts(),
        response: crate::review_attention_wire::ResponseContext {
            inferred_nesting_budget: DEPTH,
        },
    }
}
fn mint_lease() -> Result<cannery_core::principal::Secret, cannery_identity::error::IdentityError> {
    Ok(cannery_identity::secrets::new_secret("cr_lease_")?
        .plaintext()
        .clone())
}
fn mint_job() -> Result<cannery_core::principal::Secret, cannery_identity::error::IdentityError> {
    Ok(cannery_identity::secrets::new_secret("cr_job_")?
        .plaintext()
        .clone())
}
fn mint_upload() -> Result<cannery_core::principal::Secret, cannery_identity::error::IdentityError>
{
    Ok(cannery_identity::secrets::new_secret("cr_upl_")?
        .plaintext()
        .clone())
}
fn nonce() -> Result<String, cannery_identity::error::IdentityError> {
    Ok(cannery_identity::secrets::new_secret("")?
        .plaintext()
        .expose()
        .to_owned())
}

pub(super) fn provider(settings: &Settings) -> Result<Option<Arc<dyn OidcProvider>>, ServerError> {
    let Some(issuer) = &settings.auth.oidc_issuer else {
        return Ok(None);
    };
    let config = ClientConfig {
        issuer: String::from(issuer),
        client_id: String::from(
            settings
                .auth
                .oidc_client_id
                .as_deref()
                .ok_or(ServerError::OidcConfiguration)?,
        ),
        client_secret: cannery_core::principal::Secret::new(
            settings
                .auth
                .oidc_client_secret
                .as_ref()
                .ok_or(ServerError::OidcConfiguration)?
                .expose()
                .to_owned(),
        ),
        redirect_uri: String::from(&format!(
            "{}/auth/callback",
            settings.server.public_base_url.trim_end_matches('/')
        )),
        scopes: String::from(&settings.auth.oidc_scopes),
    };
    Ok(Some(Arc::new(
        OidcClient::new(
            config,
            Arc::new(HttpTransport::new().map_err(|_| ServerError::OidcConfiguration)?),
            Arc::new(RingVerifier),
            Arc::new(SystemMonotonicClock::default()),
            RenderingContext::DEFAULT,
        )
        .map_err(|_| ServerError::OidcConfiguration)?,
    )))
}

#[allow(
    clippy::too_many_lines,
    reason = "All production route policies are assembled together"
)]
pub(super) fn contexts(
    store: &Arc<ObjectStore>,
    settings: &cannery_core::settings::Settings,
) -> Result<(RouteContexts, Arc<crate::upload_routes::CancellationTasks>), ServerError> {
    let cancellation = crate::upload_routes::CancellationTasks::new();
    let response = crate::attempt_read_wire::ResponseContext {
        inferred_nesting_budget: DEPTH,
    };
    let download: Arc<dyn crate::artifact_download::DownloadStore> = store.clone();
    let lifecycle = Arc::new(crate::job_lifecycle::Context {
        jobs: jobs(),
        attempts: attempts(),
        config: config(),
        hypotheses: hypotheses(),
        rendering: science(),
    });
    let release = Arc::new(crate::attempt_release_routes::AttemptReleaseContext {
        configuration: config(),
        science: science(),
        response,
    });
    let uploads = Arc::new(crate::upload_routes::UploadContext {
        cancellation: cancellation.clone(),
        settlement_timeout: Duration::from_secs(20),
        repository: attempts(),
        store: store.clone(),
        signing_clock: SystemTime::now,
        mint: mint_upload,
        nonce,
    });
    let job_lifecycle = Arc::new(crate::job_completion_routes::JobLifecycleContext {
        flow: (*lifecycle).clone(),
        contracts: validator()?,

        repr_budget: DEPTH,
        response: crate::job_read_wire::ResponseContext {
            inferred_nesting_budget: DEPTH,
            representation_budget: DEPTH,
        },
    });
    let slots = settings
        .storage
        .max_concurrent_validations
        .to_usize("storage.max_concurrent_validations")?;
    let json_max_bytes = settings
        .storage
        .validate_json_max_bytes
        .to_usize("storage.validate_json_max_bytes")?;
    if !(1..=256).contains(&slots) || !(1..=64 * 1024 * 1024).contains(&json_max_bytes) {
        return Err(ServerError::OutputValidationLimits);
    }
    let contexts = RouteContexts {
        job_uploads: Some(Arc::new(crate::job_upload_routes::JobUploadContext {
            lifecycle: job_lifecycle.clone(),
            uploads: uploads.clone(),
            json_max_bytes,
            validation_slots: Arc::new(tokio::sync::Semaphore::new(slots)),
            validation_timeout: Duration::from_secs(20),
        })),
        job_lifecycle: Some(job_lifecycle),
        sweeps: Some(Arc::new(crate::sweeps::SweepContext {
            store: store.clone(),
            lifecycle: lifecycle.clone(),
            release: release.clone(),
            max_stream_seconds: f64::from(
                u32::try_from(
                    settings
                        .storage
                        .max_stream_seconds
                        .to_u64("storage.max_stream_seconds")?,
                )
                .map_err(|_| ServerError::RecoveryLimits)?,
            ),
        })),
        submissions: Some(Arc::new(crate::submission_routes::SubmissionContext {
            lifecycle,
            contracts: validator()?,
            nesting_budget: DEPTH,
            response: crate::attempt_read_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
            },
        })),
        steps: Some(Arc::new(crate::step_routes::StepContext {
            contracts: validator()?,
            config: config(),
            rendering: science(),
            nesting_budget: DEPTH,
        })),
        manifests: Some(Arc::new(crate::manifest_routes::ManifestContext {
            repository: attempts(),
            contracts: validator()?,
            nesting_budget: DEPTH,
        })),
        config: Some(Arc::new(crate::config_routes::ConfigContext {
            contracts: validator()?,
            validation_walk_budget: DEPTH,
            repr_budget: DEPTH,

            rendering: science(),
            repository: config(),
            response: crate::config_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
            },
        })),
        tracks: Some(Arc::new(crate::track_routes::TrackContext {
            contracts: validator()?,
            validation_walk_budget: DEPTH,
            repr_budget: DEPTH,

            rendering: science(),
            repository: tracks(),
            config_repository: config(),
            audit_encode_budget: DEPTH,
            audit_decode_budget: DEPTH,
            response: crate::track_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
            },
        })),
        hypotheses: Some(Arc::new(crate::hypothesis_routes::HypothesisContext {
            mutations: Some(Arc::new(crate::hypothesis_mutations::MutationContext {
                science: science(),
                config: config(),
                tracks: tracks(),
                mention_walk_budget: DEPTH,
            })),
            contracts: validator()?,
            validation_walk_budget: DEPTH,
            repr_budget: DEPTH,

            repository: hypotheses(),
            response: crate::hypothesis_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
            },
            request_hash_budget: DEPTH,
        })),
        attempt_reads: Some(Arc::new(crate::attempt_read_routes::AttemptReadContext {
            repository: attempts(),
            response,
            hypothesis_lookup: crate::attempt_read_lookup::LookupContext::BorrowedUnnamed,
        })),
        attempt_leases: Some(Arc::new(crate::attempt_lease_routes::AttemptLeaseContext {
            repository: attempts(),
            release: Some(release),
        })),
        attempt_claims: Some(Arc::new(crate::attempt_claim_routes::AttemptClaimContext {
            repository: attempts(),
            configuration: config(),
            rendering: science(),
            manifest_decode_budget: DEPTH,
            audit_budget: DEPTH,
            response,
            equality: Arc::new(crate::step_binding::CheckedOutputEquality),
            mint: mint_lease,
        })),
        job_claims: Some(Arc::new(crate::job_claim_routes::JobClaimContext {
            jobs: jobs(),
            attempts: attempts(),
            rendering: science(),
            hash_budget: DEPTH,
            response_budget: DEPTH,
            mint: mint_job,
        })),
        job_reads: Some(Arc::new(crate::job_read_routes::JobReadContext {
            jobs: jobs(),
            attempts: attempts(),
            response: crate::job_read_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
                representation_budget: DEPTH,
            },
        })),
        job_inputs: Some(Arc::new(crate::job_input_routes::JobInputContext {
            jobs: jobs(),
            attempts: attempts(),
            inferred_nesting_budget: DEPTH,
            representation_budget: DEPTH,
            store: download.clone(),
            signing_clock: SystemTime::now,
        })),
        predecessor_input: Some(Arc::new(
            crate::predecessor_input_routes::PredecessorInputContext {
                repository: attempts(),
                rendering: science(),
                manifest_decode_budget: DEPTH,
                store: download.clone(),
                signing_clock: SystemTime::now,
            },
        )),
        download: Some(Arc::new(crate::artifact_download::DownloadContext {
            store: download,
            signing_clock: SystemTime::now,
            repository: attempts(),
        })),
        uploads: Some(uploads),
        reports: Some(Arc::new(crate::report_routes::ReportContext {
            reports: cannery_comments_reports::reports::JsonContext {
                decode_nesting_budget: DEPTH,
            },
            attempts: attempts(),
            hypotheses: hypotheses(),
            response: crate::report_wire::ResponseContext {
                inferred_nesting_budget: DEPTH,
                representation_budget: DEPTH,
            },
        })),
        comments: Some(Arc::new(crate::comment_routes::CommentContext {
            hypotheses: hypotheses(),
            mutations: Some(Arc::new(crate::comment_mutations::CommentMutationContext {
                mention_walk_budget: DEPTH,
            })),
        })),
        review_attention: Some(Arc::new(review_reads())),
        review_decisions: Some(Arc::new(
            crate::review_decision_routes::ReviewDecisionContext {
                reads: review_reads(),
                contracts: validator()?,
                validation_walk_budget: DEPTH,
                repr_budget: DEPTH,

                request_hash_budget: DEPTH,
                config: config(),
                science_rendering: science(),
                jobs: jobs(),
                audit_encoding_budget: DEPTH,
            },
        )),
        search: Some(Arc::new(crate::search_routes::SearchContext {
            cursor_decode_budget: DEPTH,
        })),
        metrics: Some(Arc::new(crate::metric_routes::MetricContext {
            repository: cannery_metrics::repo::JsonContext {
                nesting_budget: DEPTH,
            },
            config_repository: config(),
            science: science(),
            rendering_budget: DEPTH,
            response: cannery_metrics::projection::Context {
                inferred_nesting_budget: DEPTH,
            },
        })),
    };
    Ok((contexts, cancellation))
}
