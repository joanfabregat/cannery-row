//! Injected orchestration tests: these stubs do not establish HTTP or crypto parity.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::{
    json::{Document, Node, decode},
    principal::Secret,
};
use cannery_server::{
    oidc_claims::Identity,
    oidc_client::{
        BasicCredentials, ClientConfig, JsonTransport, MonotonicClock, OidcClient,
        SigningKeySource, TokenVerifier, TransportError, VerificationContext,
    },
    oidc_protocol::{RenderingContext, pkce_challenge},
    oidc_provider::{OidcProvider, ProviderError},
};
use futures_util::future::BoxFuture;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

fn text(value: &str) -> String {
    String::from(value)
}
fn reference(path: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(runtime_reference!(
        "/../../crates/server/tests/fixtures/oidc_client_reference.json"
    ))
    .unwrap()
    .pointer(path)
    .unwrap()
    .clone()
}
fn assert_source_urls(transport: &Transport, path: &str) {
    let observed = reference(path);
    let expected = observed["request_urls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| text(value.as_str().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(transport.get_urls(), expected);
}
fn doc(value: &str) -> Arc<Document> {
    Arc::new(decode(value.as_bytes(), cannery_core::json::MAX_DEPTH).unwrap())
}
fn metadata(tag: &str) -> Arc<Document> {
    doc(&format!(
        r#"{{"issuer":"https://provider","authorization_endpoint":"https://provider/auth-{tag}?old=1","token_endpoint":"https://provider/token-{tag}","jwks_uri":"https://provider/jwks-{tag}","end_session_endpoint":"https://provider/logout-{tag}"}}"#
    ))
}
fn field(document: &Document, key: &str) -> String {
    match document
        .node(document.field(document.root(), key).unwrap())
        .unwrap()
    {
        Node::String(value) => value.clone(),
        _ => panic!("fixed fixture string missing"),
    }
}
struct Clock(AtomicU64);
impl Clock {
    fn new(now: f64) -> Self {
        Self(AtomicU64::new(now.to_bits()))
    }
    fn set(&self, now: f64) {
        self.0.store(now.to_bits(), Ordering::SeqCst);
    }
}
impl MonotonicClock for Clock {
    fn now(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::SeqCst))
    }
}
#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}
impl Gate {
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.entered.notified())
            .await
            .expect("independent request never started");
    }
}
struct Step {
    reply: Result<Arc<Document>, TransportError>,
    gate: Option<Arc<Gate>>,
}
impl Step {
    fn json(document: Arc<Document>) -> Self {
        Self {
            reply: Ok(document),
            gate: None,
        }
    }
    fn fail(error: TransportError) -> Self {
        Self {
            reply: Err(error),
            gate: None,
        }
    }
    fn gated(document: Arc<Document>, gate: &Arc<Gate>) -> Self {
        Self {
            reply: Ok(document),
            gate: Some(Arc::clone(gate)),
        }
    }
}
enum Request {
    Get(String),
    Post {
        url: String,
        form: Secret,
        client_id: String,
        client_secret: Secret,
    },
}
struct Transport {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<Request>>,
}
impl Transport {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn get_urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|request| match request {
                Request::Get(url) => Some(url.to_owned()),
                Request::Post { .. } => None,
            })
            .collect()
    }
    fn next(&self, request: Request) -> Step {
        self.requests.lock().unwrap().push(request);
        self.steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider request")
    }
}
async fn reply(step: Step) -> Result<Arc<Document>, TransportError> {
    if let Some(gate) = step.gate {
        gate.entered.notify_one();
        gate.release.notified().await;
    }
    step.reply
}
impl JsonTransport for Transport {
    fn prepare_post_url(&self, url: &str) -> Result<(), TransportError> {
        // Explicit fixture preparation, not a production URL parser.
        if url == "http://host:invalid/token" {
            Err(TransportError::Unhandled)
        } else {
            Ok(())
        }
    }
    fn get_json<'a>(
        &'a self,
        url: &'a str,
    ) -> BoxFuture<'a, Result<Arc<Document>, TransportError>> {
        let step = self.next(Request::Get(url.to_owned()));
        Box::pin(reply(step))
    }
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Secret,
        credentials: BasicCredentials<'a>,
    ) -> BoxFuture<'a, Result<Arc<Document>, TransportError>> {
        let step = self.next(Request::Post {
            url: url.to_owned(),
            form: form.clone(),
            client_id: credentials.client_id.clone(),
            client_secret: credentials.client_secret.clone(),
        });
        Box::pin(reply(step))
    }
}
#[derive(Default)]
struct Verifier {
    calls: AtomicUsize,
    inputs: Mutex<Vec<(String, Secret)>>,
}
impl TokenVerifier for Verifier {
    fn verify_id_token<'a>(
        &'a self,
        token: &'a str,
        nonce: &'a Secret,
        context: VerificationContext<'a>,
        _: &'a dyn SigningKeySource,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs
            .lock()
            .unwrap()
            .push((token.to_owned(), nonce.clone()));
        Box::pin(async move {
            Ok(Identity {
                issuer: context.issuer.clone(),
                subject: text("test-stub-only"),
                email: None,
                email_verified: false,
                name: None,
            })
        })
    }
}
fn config() -> ClientConfig {
    ClientConfig {
        issuer: text("https://provider///"),
        client_id: text("client é"),
        client_secret: Secret::new("synthetic-password".into()),
        redirect_uri: text("https://app/cb"),
        scopes: text("openid email"),
    }
}
fn client_with(
    config: ClientConfig,
    transport: &Arc<Transport>,
    clock: &Arc<Clock>,
    verifier: &Arc<Verifier>,
) -> Arc<OidcClient> {
    Arc::new(
        OidcClient::new(
            config,
            transport.clone(),
            verifier.clone(),
            clock.clone(),
            RenderingContext::DEFAULT,
        )
        .unwrap(),
    )
}
fn client(transport: &Arc<Transport>, clock: &Arc<Clock>) -> Arc<OidcClient> {
    client_with(config(), transport, clock, &Arc::new(Verifier::default()))
}
fn rejected<T>(result: Result<T, ProviderError>) {
    assert!(matches!(&result, Err(ProviderError::Rejected(_))));
    drop(result);
}
fn unhandled<T>(result: Result<T, ProviderError>) {
    assert!(matches!(&result, Err(ProviderError::Unhandled)));
    drop(result);
}

#[tokio::test]
async fn authorization_has_real_source_entropy_and_ordered_pkce_query() {
    let transport = Arc::new(Transport::new(vec![Step::json(metadata("a"))]));
    let client = client(&transport, &Arc::new(Clock::new(100.0)));
    let request = client.authorization_request().await.unwrap();
    for secret in [&request.state, &request.nonce] {
        assert_eq!(secret.expose().len(), 43);
        assert_eq!(URL_SAFE_NO_PAD.decode(secret.expose()).unwrap().len(), 32);
    }
    assert_eq!(request.code_verifier.expose().len(), 86);
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(request.code_verifier.expose())
            .unwrap()
            .len(),
        64
    );
    assert_ne!(request.state.expose(), request.nonce.expose());
    assert_eq!(
        request.url,
        text(&format!(
            "https://provider/auth-a?old=1?response_type=code&client_id=client+%C3%A9&redirect_uri=https%3A%2F%2Fapp%2Fcb&scope=openid+email&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
            request.state.expose(),
            request.nonce.expose(),
            pkce_challenge(&request.code_verifier)
        ))
    );
    let second = client.authorization_request().await.unwrap();
    assert_ne!(request.state.expose(), second.state.expose());
    assert_eq!(
        transport.get_urls(),
        vec![text("https://provider/.well-known/openid-configuration")]
    );
    assert_eq!(format!("{request:?}"), "AuthorizationRequest([redacted])");
    assert_eq!(format!("{client:?}"), "OidcClient([redacted])");
}

#[tokio::test]
async fn exchange_orders_form_credentials_and_verifier_after_discovery_and_response() {
    let transport = Arc::new(Transport::new(vec![
        Step::json(metadata("a")),
        Step::json(doc(r#"{"id_token":"synthetic-token"}"#)),
    ]));
    let verifier = Arc::new(Verifier::default());
    let client = client_with(
        config(),
        &transport,
        &Arc::new(Clock::new(100.0)),
        &verifier,
    );
    let result = client
        .exchange_code(
            &text("c +é"),
            &Secret::new("verifier&x".into()),
            &Secret::new("nonce".into()),
        )
        .await
        .unwrap();
    assert_eq!(result.issuer, text("https://provider"));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    let requests = transport.requests.lock().unwrap();
    let Request::Post {
        url,
        form,
        client_id,
        client_secret,
    } = &requests[1]
    else {
        panic!("POST missing")
    };
    assert_eq!(url, &text("https://provider/token-a"));
    assert_eq!(
        form.expose(),
        "grant_type=authorization_code&code=c+%2B%C3%A9&redirect_uri=https%3A%2F%2Fapp%2Fcb&code_verifier=verifier%26x"
    );
    assert_eq!(client_id, &text("client é"));
    assert_eq!(client_secret.expose(), "synthetic-password");
    assert_eq!(format!("{form:?}"), "Secret([redacted])");
    let inputs = verifier.inputs.lock().unwrap();
    assert_eq!(inputs[0].0, text("synthetic-token"));
    assert_eq!(inputs[0].1.expose(), "nonce");
}

#[tokio::test]
async fn missing_token_and_transport_error_classes_do_not_invoke_verifier() {
    for response in [
        "{}",
        "[]",
        "null",
        "false",
        r#"{"id_token":null}"#,
        r#"{"id_token":4}"#,
    ] {
        let transport = Arc::new(Transport::new(vec![
            Step::json(metadata("a")),
            Step::json(doc(response)),
        ]));
        let verifier = Arc::new(Verifier::default());
        let client = client_with(config(), &transport, &Arc::new(Clock::new(0.0)), &verifier);
        rejected(
            client
                .exchange_code(
                    &text("code"),
                    &Secret::new("v".into()),
                    &Secret::new("n".into()),
                )
                .await,
        );
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    }
    for error in [TransportError::Rejected, TransportError::Unhandled] {
        for post in [false, true] {
            let mut steps = vec![];
            if post {
                steps.push(Step::json(metadata("a")));
            }
            steps.push(Step::fail(error));
            let transport = Arc::new(Transport::new(steps));
            let client = client(&transport, &Arc::new(Clock::new(0.0)));
            let result = client
                .exchange_code(
                    &text("code"),
                    &Secret::new("v".into()),
                    &Secret::new("n".into()),
                )
                .await;
            match error {
                TransportError::Rejected => rejected(result),
                TransportError::Unhandled => unhandled(result),
            }
        }
    }
}

#[tokio::test]
async fn ttl_boundary_backward_clock_and_failed_refresh_preserve_cached_state() {
    let a = metadata("a");
    let jwks = doc(r#"{"keys":[],"tag":"a"}"#);
    let transport = Arc::new(Transport::new(vec![
        Step::json(Arc::clone(&a)),
        Step::json(Arc::clone(&jwks)),
        Step::fail(TransportError::Rejected),
        Step::json(metadata("b")),
    ]));
    let clock = Arc::new(Clock::new(100.0));
    let client = client(&transport, &clock);
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    let lookup = client.begin_signing_keys().await.unwrap();
    assert!(Arc::ptr_eq(&lookup.document(false).await.unwrap(), &jwks));
    clock.set(3700.0);
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    clock.set(99.0);
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    clock.set(3700.001);
    rejected(client.metadata().await);
    clock.set(100.0);
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    assert!(Arc::ptr_eq(&lookup.document(false).await.unwrap(), &jwks));
    assert_source_urls(&transport, "/failure_and_ttl");
    assert_eq!(
        reference("/failure_and_ttl")["failure"]["class"],
        "Rejected"
    );
    clock.set(3700.001);
    assert_eq!(
        field(&client.metadata().await.unwrap(), "token_endpoint"),
        text("https://provider/token-b")
    );
    assert_eq!(transport.count(), 4);
}

#[tokio::test]
async fn metadata_time_is_sampled_at_successful_completion() {
    let gate = Arc::new(Gate::default());
    let a = metadata("a");
    let transport = Arc::new(Transport::new(vec![
        Step::gated(Arc::clone(&a), &gate),
        Step::json(metadata("b")),
    ]));
    let clock = Arc::new(Clock::new(100.0));
    let client = client(&transport, &clock);
    let task = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.metadata().await }
    });
    gate.entered().await;
    clock.set(200.0);
    gate.release.notify_one();
    task.await.unwrap().unwrap();
    clock.set(3800.0);
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    clock.set(3800.001);
    client.metadata().await.unwrap();
    assert_eq!(transport.count(), 2);
}

#[tokio::test]
async fn independent_metadata_misses_last_completion_wins_and_invalidates_jwks() {
    let first = Arc::new(Gate::default());
    let second = Arc::new(Gate::default());
    let a = metadata("a");
    let b = metadata("b");
    let transport = Arc::new(Transport::new(vec![
        Step::gated(Arc::clone(&a), &first),
        Step::gated(Arc::clone(&b), &second),
        Step::json(doc(r#"{"tag":"b"}"#)),
        Step::json(doc(r#"{"tag":"a"}"#)),
    ]));
    let clock = Arc::new(Clock::new(100.0));
    let client = client(&transport, &clock);
    let one = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.metadata().await }
    });
    first.entered().await;
    let two = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.metadata().await }
    });
    second.entered().await;
    clock.set(110.0);
    second.release.notify_one();
    assert!(Arc::ptr_eq(&two.await.unwrap().unwrap(), &b));
    assert_eq!(
        field(
            &client
                .begin_signing_keys()
                .await
                .unwrap()
                .document(false)
                .await
                .unwrap(),
            "tag"
        ),
        text("b")
    );
    clock.set(120.0);
    first.release.notify_one();
    assert!(Arc::ptr_eq(&one.await.unwrap().unwrap(), &a));
    assert!(Arc::ptr_eq(&client.metadata().await.unwrap(), &a));
    assert_eq!(
        field(
            &client
                .begin_signing_keys()
                .await
                .unwrap()
                .document(false)
                .await
                .unwrap(),
            "tag"
        ),
        text("a")
    );
    assert_eq!(
        transport.get_urls(),
        vec![
            text("https://provider/.well-known/openid-configuration"),
            text("https://provider/.well-known/openid-configuration"),
            text("https://provider/jwks-b"),
            text("https://provider/jwks-a")
        ]
    );
    assert_source_urls(&transport, "/metadata_race");
    assert_eq!(
        field(&client.metadata().await.unwrap(), "token_endpoint")
            .as_utf8()
            .unwrap(),
        reference("/metadata_race")["last_token_endpoint"]
            .as_str()
            .unwrap()
    );
}

#[tokio::test]
async fn jwks_misses_are_independent_and_failed_force_refresh_keeps_old_cache() {
    let first = Arc::new(Gate::default());
    let second = Arc::new(Gate::default());
    let a = doc(r#"{"tag":"first"}"#);
    let b = doc(r#"{"tag":"second"}"#);
    let transport = Arc::new(Transport::new(vec![
        Step::json(metadata("a")),
        Step::gated(Arc::clone(&a), &first),
        Step::gated(Arc::clone(&b), &second),
        Step::fail(TransportError::Rejected),
    ]));
    let client = client(&transport, &Arc::new(Clock::new(100.0)));
    client.metadata().await.unwrap();
    let one = tokio::spawn({
        let client = Arc::clone(&client);
        async move {
            client
                .begin_signing_keys()
                .await
                .unwrap()
                .document(false)
                .await
        }
    });
    first.entered().await;
    let two = tokio::spawn({
        let client = Arc::clone(&client);
        async move {
            client
                .begin_signing_keys()
                .await
                .unwrap()
                .document(false)
                .await
        }
    });
    second.entered().await;
    second.release.notify_one();
    assert!(Arc::ptr_eq(&two.await.unwrap().unwrap(), &b));
    first.release.notify_one();
    assert!(Arc::ptr_eq(&one.await.unwrap().unwrap(), &a));
    let lookup = client.begin_signing_keys().await.unwrap();
    rejected(lookup.document(true).await);
    assert!(Arc::ptr_eq(&lookup.document(false).await.unwrap(), &a));
    assert_eq!(transport.count(), 4);
}

#[tokio::test]
async fn old_signing_lookup_keeps_its_uri_when_successful_discovery_resets_jwks() {
    let transport = Arc::new(Transport::new(vec![
        Step::json(metadata("a")),
        Step::json(doc(r#"{"tag":"old"}"#)),
        Step::json(metadata("b")),
        Step::json(doc(r#"{"tag":"old-refetched"}"#)),
    ]));
    let clock = Arc::new(Clock::new(100.0));
    let client = client(&transport, &clock);
    let old = client.begin_signing_keys().await.unwrap();
    old.document(false).await.unwrap();
    clock.set(3700.001);
    let new = client.begin_signing_keys().await.unwrap();
    assert_eq!(
        field(&old.document(false).await.unwrap(), "tag"),
        text("old-refetched")
    );
    assert_eq!(
        field(&new.document(false).await.unwrap(), "tag"),
        text("old-refetched")
    );
    assert_eq!(
        transport.get_urls().last(),
        Some(&text("https://provider/jwks-a"))
    );
}

#[tokio::test]
async fn metadata_validation_and_logout_preserve_error_and_encoding_order() {
    for body in [
        "[]",
        "null",
        r#"{"issuer":"wrong"}"#,
        r#"{"issuer":"https://provider","authorization_endpoint":null}"#,
    ] {
        let transport = Arc::new(Transport::new(vec![Step::json(doc(body))]));
        rejected(
            client(&transport, &Arc::new(Clock::new(0.0)))
                .metadata()
                .await,
        );
    }
    let transport = Arc::new(Transport::new(vec![Step::json(metadata("a"))]));
    let native = client(&transport, &Arc::new(Clock::new(0.0)));
    assert_eq!(
        native
            .end_session_url(&text("https://app/bye é"))
            .await
            .unwrap(),
        Some(text(
            "https://provider/logout-a?client_id=client+%C3%A9&post_logout_redirect_uri=https%3A%2F%2Fapp%2Fbye+%C3%A9"
        ))
    );
    assert!(cannery_core::text::from_codepoints(vec![0xd800]).is_none());
    assert!(decode(br#""\ud800""#, 64).is_err());
    assert_source_urls(&transport, "/encoding/safe_logout");
    let transport = Arc::new(Transport::new(vec![Step::json(doc(
        r#"{"issuer":"https://provider","authorization_endpoint":"x","token_endpoint":"y","jwks_uri":"z","end_session_endpoint":"http://remote/logout"}"#,
    ))]));
    let native = client(&transport, &Arc::new(Clock::new(0.0)));
    assert!(
        native
            .end_session_url("https://app/bye é")
            .await
            .unwrap()
            .is_none()
    );
    assert_source_urls(&transport, "/encoding/unsafe_logout");
    assert_eq!(
        reference("/encoding/unsafe_logout")["outcome"]["none"],
        true
    );
}

#[tokio::test]
async fn invalid_discovery_and_jwks_refresh_leave_existing_caches_intact() {
    let a = metadata("a");
    let old = doc(r#"{"tag":"old"}"#);
    let transport = Arc::new(Transport::new(vec![
        Step::json(Arc::clone(&a)),
        Step::json(Arc::clone(&old)),
        Step::json(doc(r#"{"issuer":"wrong"}"#)),
        Step::json(doc("[]")),
    ]));
    let clock = Arc::new(Clock::new(100.0));
    let native = client(&transport, &clock);
    let lookup = native.begin_signing_keys().await.unwrap();
    lookup.document(false).await.unwrap();
    clock.set(3700.001);
    rejected(native.metadata().await);
    clock.set(100.0);
    assert!(Arc::ptr_eq(&native.metadata().await.unwrap(), &a));
    rejected(lookup.document(true).await);
    assert!(Arc::ptr_eq(&lookup.document(false).await.unwrap(), &old));
    assert_eq!(transport.count(), 4);
}

struct ErrorVerifier(bool);
impl TokenVerifier for ErrorVerifier {
    fn verify_id_token<'a>(
        &'a self,
        _: &'a str,
        _: &'a Secret,
        _: VerificationContext<'a>,
        _: &'a dyn SigningKeySource,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            Err(if self.0 {
                ProviderError::Unhandled
            } else {
                ProviderError::Rejected(text("test rejection"))
            })
        })
    }
}
#[tokio::test]
async fn verifier_rejection_and_unhandled_failure_propagate_after_token_response() {
    for fatal in [false, true] {
        let transport = Arc::new(Transport::new(vec![
            Step::json(metadata("a")),
            Step::json(doc(r#"{"id_token":"synthetic-token"}"#)),
        ]));
        let native = OidcClient::new(
            config(),
            transport.clone(),
            Arc::new(ErrorVerifier(fatal)),
            Arc::new(Clock::new(0.0)),
            RenderingContext::DEFAULT,
        )
        .unwrap();
        let result = native
            .exchange_code(
                &text("code"),
                &Secret::new("v".into()),
                &Secret::new("n".into()),
            )
            .await;
        if fatal {
            unhandled(result);
        } else {
            rejected(result);
        }
        assert_eq!(transport.count(), 2);
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
