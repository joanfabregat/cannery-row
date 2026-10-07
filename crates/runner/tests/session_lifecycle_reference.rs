//! Replay actual frozen `JobSession` callback observations.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, Node, NodeId};
use cannery_runner::{
    launcher::PosixPath,
    session_lifecycle::{
        Action, Callbacks, Endpoint, ExceptionKind, Input, JobFailure, Lost, Post, Response,
        Session, SessionError, SessionFuture, SessionOperation, SharedState,
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;

fn text(value: &str) -> String {
    String::from(value)
}
fn doc(value: &Value) -> Arc<Document> {
    Arc::new(json::decode(&serde_json::to_vec(value).unwrap(), 1000).unwrap())
}
fn value(document: &Document) -> Value {
    serde_json::from_str(&json::encode_ascii_pretty(document, 1000).unwrap()).unwrap()
}
fn failure() -> JobFailure {
    JobFailure {
        code: text("step_error"),
        reason: text("failed"),
        step: Some(text("score")),
        logs: Some(doc(&json!([{"name":"stderr"}]))),
    }
}
fn error(name: &str) -> SessionError {
    let (kind, description) = match name {
        "http" => (ExceptionKind::Http, "ConnectError: wire"),
        "runner" => (ExceptionKind::Runner, "RunnerError: operation"),
        "value" => (ExceptionKind::Value, "ValueError: decode"),
        "key" => (ExceptionKind::Key, "KeyError: 'state'"),
        "type" => (ExceptionKind::Type, "TypeError: decode"),
        "recursion" => (ExceptionKind::Recursion, "RecursionError: decode"),
        "ordinary" => (ExceptionKind::Ordinary, "RuntimeError: operation"),
        "fatal" => (ExceptionKind::Fatal, "BaseException: fatal"),
        "cancelled" => return SessionError::Cancelled,
        "failed" => return SessionError::Failed(failure()),
        "stopped" => {
            return SessionError::Stopped {
                state: text("abandoned"),
                detail: doc(&json!({"stopped":true})),
            };
        }
        _ => panic!("unknown fixture error"),
    };
    SessionError::exception(kind, text(description))
}
fn raised(error: &SessionError) -> Value {
    match error {
        SessionError::Cancelled => {
            json!({"raised":"CancelledError","description":"CancelledError"})
        }
        SessionError::Failed(failure) => {
            json!({"raised":"JobFailed","description":format!("JobFailed: {}",failure.reason.as_utf8().unwrap())})
        }
        SessionError::Stopped { state, .. } => {
            json!({"raised":"Stopped","description":format!("Stopped: {}",state.as_utf8().unwrap())})
        }
        SessionError::Exception { description, .. } => {
            let description = description.as_utf8().unwrap();
            json!({"raised":description.split(':').next().unwrap(),"description":description})
        }
    }
}
struct Ready<T: Send> {
    result: Option<Result<T, SessionError>>,
}
impl<T: Send> SessionOperation<T> for Ready<T> {
    fn cancel(&self) {}
    fn settle(&mut self) -> SessionFuture<'_, T> {
        Box::pin(async { self.result.take().unwrap() })
    }
}
fn ready<T: Send + 'static>(result: Result<T, SessionError>) -> Box<dyn SessionOperation<T>> {
    Box::new(Ready {
        result: Some(result),
    })
}
#[derive(Default)]
struct Sleep {
    cancelled: AtomicBool,
    notify: Notify,
}
impl SessionOperation<()> for Sleep {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
    fn settle(&mut self) -> SessionFuture<'_, ()> {
        Box::pin(async {
            let changed = self.notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.cancelled.load(Ordering::Acquire) {
                changed.await;
            }
            Err(SessionError::Cancelled)
        })
    }
}
struct ObservedResponse {
    recipe: Value,
    trace: Arc<Mutex<Vec<Value>>>,
    decodes: AtomicUsize,
    endpoint: Endpoint,
}
impl Response for ObservedResponse {
    fn status(&self) -> u16 {
        u16::try_from(
            self.recipe
                .get("status")
                .and_then(Value::as_u64)
                .unwrap_or(200),
        )
        .unwrap()
    }
    fn json(&self) -> Result<Arc<Document>, SessionError> {
        let index = self.decodes.fetch_add(1, Ordering::AcqRel);
        self.trace.lock().unwrap().push(json!(["json", index + 1]));
        if let Some(name) = self
            .recipe
            .get("json_errors")
            .and_then(|errors| errors.get(index))
            .and_then(Value::as_str)
        {
            return Err(error(name));
        }
        let result = if let Some(values) = self.recipe.get("json_values").and_then(Value::as_array)
        {
            values.get(index.min(values.len() - 1)).unwrap().clone()
        } else {
            self.recipe
                .get("json")
                .cloned()
                .unwrap_or_else(|| json!({"state":"completed"}))
        };
        Ok(doc(&result))
    }
    fn error_description(&self, code: Option<&String>) -> String {
        let code = code
            .and_then(String::as_utf8)
            .filter(|code| !code.is_empty())
            .unwrap_or_else(|| "no error code".to_owned());
        let endpoint = match self.endpoint {
            Endpoint::Completion => "completion",
            Endpoint::Failure => "failure",
        };
        text(&format!(
            "RunnerError: POST /jobs/fixture/{endpoint} answered HTTP {} ({code})",
            self.status()
        ))
    }
}
struct Capture {
    recipe: Value,
    trace: Arc<Mutex<Vec<Value>>>,
    posts: AtomicUsize,
}
impl Capture {
    fn trace(&self, value: Value) {
        self.trace.lock().unwrap().push(value);
    }
    fn optional_error(&self, field: &str) -> Result<(), SessionError> {
        match self.recipe.get(field).and_then(Value::as_str) {
            Some(name) => Err(error(name)),
            None => Ok(()),
        }
    }
}
impl Callbacks for Capture {
    fn heartbeat_interval(&self) -> Result<f64, SessionError> {
        match self.recipe.get("interval") {
            Some(Value::Null) => Err(SessionError::exception(
                ExceptionKind::Type,
                text(
                    "TypeError: float() argument must be a string or a real number, not 'NoneType'",
                ),
            )),
            _ => Ok(10000.0),
        }
    }
    fn make_work(&self) -> Box<dyn SessionOperation<PosixPath>> {
        self.trace(json!(["work"]));
        ready(
            self.optional_error("work_error")
                .map(|()| PosixPath::new(&text("/fixture/work"))),
        )
    }
    fn body(&self, state: SharedState, _lost: Lost) -> Box<dyn SessionOperation<Arc<Document>>> {
        self.trace(json!(["body"]));
        state.lock().unwrap().step_dirs = vec![
            (
                text("second"),
                PosixPath::new(&text("/fixture/work/second")),
            ),
            (text("first"), PosixPath::new(&text("/fixture/work/first"))),
        ];
        ready(
            self.optional_error("body_error")
                .map(|()| doc(&json!({"metric":7}))),
        )
    }
    fn sleep(&self, seconds: f64) -> Box<dyn SessionOperation<()>> {
        if seconds == 0.0 {
            ready(Ok(()))
        } else {
            Box::new(Sleep::default())
        }
    }
    fn renew(&self, interval: f64) -> Box<dyn SessionOperation<bool>> {
        self.trace(json!(["renew", interval]));
        ready(self.optional_error("renew_error").map(|()| false))
    }
    fn release_job(&self) -> Box<dyn SessionOperation<()>> {
        self.trace(json!(["release", "fixture"]));
        ready(self.optional_error("release_error"))
    }
    fn remove_work(&self, path: PosixPath) -> Box<dyn SessionOperation<()>> {
        self.trace(json!(["remove", path.text().as_utf8().unwrap()]));
        ready(self.optional_error("remove_error"))
    }
    fn post(&self, request: Post) -> Box<dyn SessionOperation<Arc<dyn Response>>> {
        let endpoint = match request.endpoint {
            Endpoint::Completion => "completion",
            Endpoint::Failure => "failure",
        };
        self.trace(json!([
            "post",
            endpoint,
            value(&request.body),
            request.idempotency_key.as_ref().and_then(String::as_utf8)
        ]));
        let index = self.posts.fetch_add(1, Ordering::AcqRel);
        let responses = self.recipe.get("responses").and_then(Value::as_array);
        let recipe = responses.map_or_else(
            || json!({}),
            |values| values[index.min(values.len() - 1)].clone(),
        );
        if let Some(name) = recipe.get("post_error").and_then(Value::as_str) {
            return ready(Err(error(name)));
        }
        ready(Ok(Arc::new(ObservedResponse {
            recipe,
            trace: self.trace.clone(),
            decodes: AtomicUsize::new(0),
            endpoint: request.endpoint,
        }) as Arc<dyn Response>))
    }
    fn log(&self, message: &str) -> Result<(), SessionError> {
        self.trace(json!(["log", message]));
        self.optional_error("log_error")
    }
    fn render(&self, document: &Document, node: NodeId) -> Result<String, SessionError> {
        Ok(text(&python_str(document, node, false)))
    }
}
fn python_str(document: &Document, node: NodeId, repr: bool) -> String {
    match document.node(node).unwrap() {
        Node::Null => "None".to_owned(),
        Node::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        Node::Integer(value) => value.to_string(),
        Node::String(value) => {
            let value = value.as_utf8().unwrap();
            if repr { format!("'{value}'") } else { value }
        }
        Node::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|&node| python_str(document, node, true))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => panic!("fixture renderer has no unmeasured types"),
    }
}
#[tokio::test]
async fn frozen_session_lifecycle_reference() {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/session_lifecycle_reference.json"
    ))
    .unwrap();
    assert_eq!(fixture["python"], "3.13.11");
    assert_eq!(fixture["unicode"], "15.1.0");
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 268);
    for (index, case) in cases.iter().enumerate() {
        let recipe = &case["recipe"];
        let mode = recipe["mode"].as_str().unwrap();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let callbacks = Arc::new(Capture {
            recipe: recipe.clone(),
            trace: trace.clone(),
            posts: AtomicUsize::new(0),
        });
        let session = Session::new(
            Input {
                job_id: text("fixture"),
                generation: text("7"),
                attempt_id: if recipe
                    .get("missing_attempt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    None
                } else {
                    Some(doc(&json!("attempt")))
                },
                keep_dirs: recipe
                    .get("keep_dirs")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            },
            callbacks,
        );
        session.state.lock().unwrap().uploaded = Some(doc(
            &json!([{"name":"evidence","path":"x","sha256":"fixture"}]),
        ));
        let result = if mode == "heartbeat" {
            session
                .clone()
                .start_heartbeat(0.0)
                .settle()
                .await
                .map(|()| None)
        } else {
            let action = match mode {
                "run" => Action::Run,
                "outcome" => Action::Outcome,
                "complete" => Action::Complete(doc(&json!({"metric":7}))),
                "fail" => Action::Fail(failure()),
                _ => panic!("unknown mode"),
            };
            session
                .clone()
                .start_action(action)
                .settle()
                .await
                .map(Some)
        };
        let outcome = match result {
            Ok(Some(result)) => {
                json!({"returned":{"job_id":result.job_id.as_utf8().unwrap(),"state":result.state.as_utf8().unwrap(),"detail":value(&result.detail),"step_dirs":result.step_dirs.iter().map(|(key,path)|json!([key.as_utf8().unwrap(),path.text().as_utf8().unwrap()])).collect::<Vec<_>>()}})
            }
            Ok(None) => json!({"returned":null}),
            Err(error) => raised(&error),
        };
        assert_eq!(outcome, case["outcome"], "case {index} outcome {recipe}");
        let actual_trace = Value::Array(trace.lock().unwrap().clone());
        assert_eq!(actual_trace, case["trace"], "case {index} trace {recipe}");
        assert_eq!(
            serde_json::to_string(&actual_trace).unwrap(),
            serde_json::to_string(&case["trace"]).unwrap(),
            "case {index} ordered payload/trace"
        );
        assert_eq!(
            session.lost.is_set(),
            case["lost"].as_bool().unwrap(),
            "case {index} lost"
        );
        assert_eq!(
            session.state.lock().unwrap().work.is_some(),
            case["work_assigned"].as_bool().unwrap(),
            "case {index} work"
        );
    }
}

// Owned callback operations for actual task-cancellation traces. Their futures
// remain owned across dropping/resuming settle, independently of this waiter.
struct AsyncOp<T: Send> {
    task: tokio::task::JoinHandle<Result<T, SessionError>>,
    cancel: Lost,
}
impl<T: Send> SessionOperation<T> for AsyncOp<T> {
    fn cancel(&self) {
        self.cancel.set();
    }
    fn settle(&mut self) -> SessionFuture<'_, T> {
        Box::pin(async { (&mut self.task).await.unwrap() })
    }
}
fn asynchronous<T: Send + 'static>(
    cancel: Lost,
    future: SessionFuture<'static, T>,
) -> Box<dyn SessionOperation<T>> {
    Box::new(AsyncOp {
        task: tokio::spawn(future),
        cancel,
    })
}
#[derive(Default)]
struct Gates {
    body: Lost,
    renew: Lost,
    renew_finished: Lost,
    release: Lost,
    release_gate: Lost,
    log: Lost,
}
struct Controlled {
    base: Arc<Capture>,
    name: String,
    gates: Arc<Gates>,
}
impl Callbacks for Controlled {
    fn heartbeat_interval(&self) -> Result<f64, SessionError> {
        Ok(
            if self.name.starts_with("beat-") || self.name == "lost-advisory" {
                0.0
            } else {
                10000.0
            },
        )
    }
    fn make_work(&self) -> Box<dyn SessionOperation<PosixPath>> {
        self.base.make_work()
    }
    fn body(&self, _state: SharedState, lost: Lost) -> Box<dyn SessionOperation<Arc<Document>>> {
        let cancel = Lost::default();
        let own_cancel = cancel.clone();
        let gates = self.gates.clone();
        let name = self.name.clone();
        let base = self.base.clone();
        asynchronous(
            cancel,
            Box::pin(async move {
                base.trace(json!(["body"]));
                gates.body.set();
                if name.starts_with("body-") || name == "before-start" {
                    own_cancel.wait().await;
                    base.trace(json!(["body_cancel"]));
                    if name == "body-cancel-error" {
                        return Err(SessionError::exception(
                            ExceptionKind::Ordinary,
                            text("RuntimeError: body cleanup"),
                        ));
                    }
                    if name != "body-swallow" {
                        return Err(SessionError::Cancelled);
                    }
                } else if name.starts_with("beat-") || name == "lost-advisory" {
                    gates.renew.wait().await;
                    if name != "beat-cancel-error" {
                        gates.renew_finished.wait().await;
                        if name == "lost-advisory" {
                            lost.wait().await;
                        } else if name == "beat-log-error" {
                            gates.log.wait().await;
                        }
                        tokio::task::yield_now().await;
                        base.trace(json!(["observed_lost", lost.is_set()]));
                    }
                }
                Ok(doc(&json!({"metric":7})))
            }),
        )
    }
    fn sleep(&self, seconds: f64) -> Box<dyn SessionOperation<()>> {
        self.base.sleep(seconds)
    }
    fn renew(&self, interval: f64) -> Box<dyn SessionOperation<bool>> {
        let cancel = Lost::default();
        let own_cancel = cancel.clone();
        let gates = self.gates.clone();
        let name = self.name.clone();
        let base = self.base.clone();
        asynchronous(
            cancel,
            Box::pin(async move {
                gates.body.wait().await;
                base.trace(json!(["renew", interval]));
                gates.renew.set();
                if name == "beat-cancel-error" {
                    own_cancel.wait().await;
                    base.trace(json!(["renew_cancel"]));
                    return Err(SessionError::exception(
                        ExceptionKind::Ordinary,
                        text("RuntimeError: beat cleanup"),
                    ));
                }
                gates.renew_finished.set();
                match name.as_str() {
                    "beat-fatal" => Err(error("fatal")),
                    "beat-log-error" => Err(error("ordinary")),
                    _ => Ok(false),
                }
            }),
        )
    }
    fn release_job(&self) -> Box<dyn SessionOperation<()>> {
        let gates = self.gates.clone();
        let name = self.name.clone();
        let base = self.base.clone();
        asynchronous(
            Lost::default(),
            Box::pin(async move {
                base.trace(json!(["release", "fixture"]));
                gates.release.set();
                if name.starts_with("release-") {
                    gates.release_gate.wait().await;
                    base.trace(json!(["release_finish"]));
                    if name == "release-cancel-error" {
                        return Err(SessionError::exception(
                            ExceptionKind::Ordinary,
                            text("RuntimeError: release cleanup"),
                        ));
                    }
                }
                Ok(())
            }),
        )
    }
    fn remove_work(&self, path: PosixPath) -> Box<dyn SessionOperation<()>> {
        self.base.remove_work(path)
    }
    fn post(&self, request: Post) -> Box<dyn SessionOperation<Arc<dyn Response>>> {
        self.base.post(request)
    }
    fn log(&self, message: &str) -> Result<(), SessionError> {
        self.base.trace(json!(["log", message]));
        self.gates.log.set();
        if self.name == "beat-log-error" {
            Err(SessionError::exception(
                ExceptionKind::Ordinary,
                text("RuntimeError: log failed"),
            ))
        } else {
            Ok(())
        }
    }
    fn render(&self, document: &Document, node: NodeId) -> Result<String, SessionError> {
        self.base.render(document, node)
    }
}
#[tokio::test]
async fn frozen_session_cancellation_and_heartbeat_reference() {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/session_lifecycle_reference.json"
    ))
    .unwrap();
    let cases = fixture["controlled"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    assert_eq!(fixture["controlled_count"], 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let gates = Arc::new(Gates::default());
        let base = Arc::new(Capture {
            recipe: json!({}),
            trace: trace.clone(),
            posts: AtomicUsize::new(0),
        });
        let callbacks = Arc::new(Controlled {
            base,
            name: name.to_owned(),
            gates: gates.clone(),
        });
        let session = Session::new(
            Input {
                job_id: text("fixture"),
                generation: text("7"),
                attempt_id: Some(doc(&json!("attempt"))),
                keep_dirs: true,
            },
            callbacks,
        );
        session.state.lock().unwrap().uploaded = Some(doc(
            &json!([{"name":"evidence","path":"x","sha256":"fixture"}]),
        ));
        let mut task = session.clone().start();
        if name == "before-start" {
            task.cancel();
        } else if name.starts_with("body-") {
            gates.body.wait().await;
            task.cancel();
        } else if name.starts_with("release-") {
            gates.release.wait().await;
            task.cancel();
            tokio::task::yield_now().await;
            task.cancel();
            tokio::task::yield_now().await;
            gates.release_gate.set();
        }
        let result = task.settle().await;
        let outcome = match result {
            Ok(result) => {
                json!({"returned":{"job_id":result.job_id.as_utf8().unwrap(),"state":result.state.as_utf8().unwrap(),"detail":value(&result.detail),"step_dirs":[]}})
            }
            Err(error) => raised(&error),
        };
        assert_eq!(outcome, case["outcome"], "{name} outcome");
        let actual_trace = Value::Array(trace.lock().unwrap().clone());
        assert_eq!(actual_trace, case["trace"], "{name} trace");
        assert_eq!(
            serde_json::to_string(&actual_trace).unwrap(),
            serde_json::to_string(&case["trace"]).unwrap(),
            "{name} ordered trace"
        );
        assert_eq!(
            session.lost.is_set(),
            case["lost"].as_bool().unwrap(),
            "{name} lost"
        );
        assert_eq!(
            session.state.lock().unwrap().work.is_some(),
            case["work_assigned"].as_bool().unwrap(),
            "{name} work"
        );
    }
}

#[tokio::test]
async fn abandoned_session_waiter_still_releases_owned_work() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let gates = Arc::new(Gates::default());
    let callbacks = Arc::new(Controlled {
        base: Arc::new(Capture {
            recipe: json!({}),
            trace: trace.clone(),
            posts: AtomicUsize::new(0),
        }),
        name: "body-cancel".to_owned(),
        gates: gates.clone(),
    });
    let session = Session::new(
        Input {
            job_id: text("fixture"),
            generation: text("7"),
            attempt_id: Some(doc(&json!("attempt"))),
            keep_dirs: true,
        },
        callbacks,
    );
    let task = session.start();
    gates.body.wait().await;
    drop(task);
    gates.release.wait().await;
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        &[
            json!(["work"]),
            json!(["body"]),
            json!(["body_cancel"]),
            json!(["release", "fixture"])
        ]
    );
}

#[tokio::test]
async fn borrowed_waiter_can_be_abandoned_resumed_and_settled_again() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let gates = Arc::new(Gates::default());
    let callbacks = Arc::new(Controlled {
        base: Arc::new(Capture {
            recipe: json!({}),
            trace: trace.clone(),
            posts: AtomicUsize::new(0),
        }),
        name: "body-cancel".to_owned(),
        gates: gates.clone(),
    });
    let session = Session::new(
        Input {
            job_id: text("fixture"),
            generation: text("7"),
            attempt_id: Some(doc(&json!("attempt"))),
            keep_dirs: true,
        },
        callbacks,
    );
    let mut task = session.start();
    {
        let waiter = task.settle();
        tokio::pin!(waiter);
        tokio::select! {
            result = &mut waiter => panic!("body unexpectedly settled: {result:?}"),
            () = gates.body.wait() => {},
        }
    }
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        &[json!(["work"]), json!(["body"])]
    );
    task.cancel();
    assert!(matches!(task.settle().await, Err(SessionError::Cancelled)));
    assert!(matches!(task.settle().await, Err(SessionError::Cancelled)));
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        &[
            json!(["work"]),
            json!(["body"]),
            json!(["body_cancel"]),
            json!(["release", "fixture"])
        ]
    );
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
