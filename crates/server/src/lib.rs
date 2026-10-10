//! HTTP application and embedded web distribution.
#![forbid(unsafe_code)]
#[cfg(test)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

mod api_contract;
mod api_model_components;
pub mod api_models;
mod application;
pub mod artifact_download;
pub mod artifact_permission;
pub mod attempt_claim_routes;
mod attempt_failure;
pub mod attempt_lease_routes;
pub mod attempt_read_lookup;
mod attempt_read_request;
pub mod attempt_read_routes;
pub mod attempt_read_wire;
mod attempt_release_request;
pub mod attempt_release_routes;
pub mod attempt_workflow;
mod authentication;
pub mod body;
mod brief_routes;
pub mod browser;
mod browser_routes;
pub mod claim_request;
pub mod comment_mutations;
mod comment_request;
pub mod comment_routes;
pub mod comparison_routes;
mod concern_routes;
pub mod config_routes;
pub mod config_wire;
#[cfg(feature = "conformance-testing")]
mod conformance_hooks;
mod context_bundle;
pub mod datetime_query;
mod decide_jobs;
mod document_jobs;
mod documentation;
pub mod errors;
pub mod health;
mod identity_routes;
mod job_claim_idempotency;
mod job_claim_request;
pub mod job_claim_routes;
mod job_completion_checks;
pub mod job_completion_routes;
pub mod job_input_routes;
pub mod job_lifecycle;
mod job_output_interface;
pub mod job_read_routes;
pub mod job_read_wire;
mod job_upload_request;
pub mod job_upload_routes;
mod job_workers;
pub mod manifest_routes;
pub mod mcp;
mod metric_request;
pub mod metric_routes;
mod metric_wire;
pub mod oidc_claims;
pub mod oidc_client;
pub mod oidc_native;
pub mod oidc_protocol;
pub mod oidc_provider;
mod plan_approval;
mod plan_routes;
mod plan_units;
pub mod predecessor_input_routes;
mod project_routes;
pub mod report_routes;
pub mod report_wire;
pub mod request_context;
pub mod requests;
pub mod review_attention_routes;
pub mod review_attention_wire;
mod review_decision_idempotency;
pub mod review_decision_routes;
mod schema_routes;
mod search_request;
pub mod search_routes;
pub mod step_binding;
pub mod step_routes;
pub mod submission_routes;
pub mod sweeps;
pub mod timestamps;
mod track_audit;
mod track_binding;
pub mod track_routes;
pub mod track_wire;
mod transport;
mod unit_idempotency;
pub mod unit_mutations;
pub mod unit_routes;
pub mod unit_wire;
mod upload_audit;
mod upload_direct;
mod upload_request;
pub mod upload_routes;
pub mod validation;
pub mod web;
mod writeup_routes;

pub use application::production_application;
pub use application::research_validation_policy;
pub use application::{
    AppState, ServerError, application, application_with_attempt_claim_context,
    application_with_attempt_lease_context, application_with_attempt_read_context,
    application_with_comment_context, application_with_config_context,
    application_with_download_context, application_with_job_claim_context,
    application_with_job_contexts, application_with_job_input_context,
    application_with_job_lifecycle_and_upload_context, application_with_job_lifecycle_context,
    application_with_job_read_and_claim_context, application_with_job_read_context,
    application_with_metric_context, application_with_oidc,
    application_with_predecessor_input_context, application_with_report_context,
    application_with_review_attention_context, application_with_review_decision_context,
    application_with_search_context, application_with_track_context, application_with_unit_context,
    application_with_upload_context, serve,
};
pub use documentation::OPENAPI;
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        browser_routes::logout,
        identity_routes::me,
        identity_routes::rest_list_tokens,
        identity_routes::rest_create_token,
        identity_routes::rest_revoke_token,
        identity_routes::rest_find_users,
        identity_routes::rest_list_service_accounts,
        identity_routes::rest_create_service_account,
        identity_routes::rest_disable_service_account,
        identity_routes::rest_list_service_tokens,
        identity_routes::rest_create_service_token,
        project_routes::list_projects,
        project_routes::create_project,
        project_routes::get_project,
        project_routes::list_members,
        project_routes::set_member,
        project_routes::remove_member,
        brief_routes::current,
        brief_routes::revise,
        brief_routes::revisions,
        brief_routes::revision,
        config_routes::create,
        config_routes::list,
        config_routes::latest,
        config_routes::specific,
        track_routes::create,
        track_routes::list,
        track_routes::read,
        track_routes::update,
        track_routes::transition,
        track_routes::history,
        unit_routes::list,
        unit_routes::read,
        unit_routes::revisions,
        unit_routes::revision,
        plan_routes::list,
        plan_routes::start,
        plan_routes::read,
        plan_routes::markdown,
        plan_routes::approach,
        plan_routes::add_unit,
        plan_routes::update_unit,
        plan_routes::drop_unit,
        plan_routes::set_alignment,
        plan_routes::set_answer,
        plan_routes::drop_answer,
        plan_routes::check,
        plan_routes::submit,
        plan_routes::review,
        plan_routes::list_track_units,
        plan_routes::unit_plan,
        plan_routes::unit_history,
        plan_routes::limits,
        plan_routes::set_limits,
        concern_routes::raise,
        concern_routes::track_list,
        concern_routes::list,
        concern_routes::read,
        concern_routes::dismiss,
        context_bundle::route,
        attempt_claim_routes::claim,
        attempt_read_routes::unit,
        attempt_read_routes::project,
        attempt_read_routes::detail,
        attempt_lease_routes::heartbeat,
        attempt_release_routes::release,
        predecessor_input_routes::input,
        upload_routes::create,
        upload_routes::rest_put_upload,
        upload_routes::rest_presign_upload,
        upload_routes::rest_finish_upload,
        manifest_routes::create,
        submission_routes::submit,
        step_routes::rest_register_producer,
        step_routes::rest_list_producers,
        step_routes::rest_get_producer,
        step_routes::rest_register_experiment_step,
        step_routes::rest_list_experiment_steps,
        step_routes::rest_get_experiment_step,
        job_claim_routes::claim,
        job_claim_routes::heartbeat,
        job_input_routes::run,
        job_input_routes::manifest,
        job_input_routes::object,
        job_upload_routes::create,
        upload_routes::rest_put_job_upload,
        upload_routes::rest_presign_job_upload,
        upload_routes::rest_finish_job_upload,
        job_completion_routes::complete,
        job_completion_routes::fail,
        job_read_routes::detail,
        job_read_routes::list,
        review_attention_routes::list,
        review_attention_routes::read,
        review_decision_routes::decide,
        writeup_routes::queue,
        writeup_routes::read,
        writeup_routes::write,
        writeup_routes::skip,
        report_routes::list,
        report_routes::detail,
        artifact_download::download,
        comment_mutations::rest_comment_on_unit,
        comment_routes::rest_list_unit_comments,
        comment_mutations::rest_comment_on_attempt,
        comment_routes::rest_list_attempt_comments,
        comment_routes::read,
        comment_mutations::edit,
        comment_routes::revisions,
        search_routes::search,
        metric_routes::catalog,
        metric_routes::query,
        metric_routes::dashboard,
        metric_routes::view,
        comparison_routes::list,
        review_attention_routes::attention,
        schema_routes::schema,
        health::health
    ),
    info(title = "Cannery Row")
)]
struct GeneratedOpenApi;

#[must_use]
pub fn generated_openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;
    let mut document = GeneratedOpenApi::openapi();
    document.merge(api_model_components::ModelComponents::openapi());
    for path in document.paths.paths.values_mut() {
        for operation in [
            &mut path.get,
            &mut path.put,
            &mut path.post,
            &mut path.delete,
            &mut path.patch,
        ]
        .into_iter()
        .flatten()
        {
            operation.tags = None;
        }
    }
    document
}
