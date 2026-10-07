//! `JobSession` lifecycle through required owned operation and response boundaries.
use crate::{failure_report, launcher::PosixPath};
use cannery_core::json::{BuildError, Document, DocumentBuilder, Node, NodeId};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionKind {
    Http,
    Runner,
    Value,
    Key,
    Type,
    Recursion,
    Ordinary,
    Fatal,
}
#[derive(Clone)]
pub struct JobFailure {
    pub code: String,
    pub reason: String,
    pub step: Option<String>,
    pub logs: Option<Arc<Document>>,
}
impl std::fmt::Debug for JobFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobFailure([redacted])")
    }
}
#[derive(Clone)]
pub enum SessionError {
    Failed(JobFailure),
    Stopped {
        state: String,
        detail: Arc<Document>,
    },
    Exception {
        kind: ExceptionKind,
        description: String,
    },
    Cancelled,
}
impl std::fmt::Debug for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exception { kind, .. } => f.debug_tuple("Exception").field(kind).finish(),
            Self::Failed(_) => f.write_str("Failed([redacted])"),
            Self::Stopped { .. } => f.write_str("Stopped([redacted])"),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}
impl SessionError {
    #[must_use]
    pub fn exception(kind: ExceptionKind, description: String) -> Self {
        Self::Exception { kind, description }
    }
    fn internal() -> Self {
        Self::exception(
            ExceptionKind::Ordinary,
            t("RuntimeError: session operation failed"),
        )
    }
    fn ordinary(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Stopped { .. })
            || matches!(self,Self::Exception{kind,..} if *kind!=ExceptionKind::Fatal)
    }
    fn reportable(&self) -> bool {
        matches!(
            self,
            Self::Exception {
                kind: ExceptionKind::Http
                    | ExceptionKind::Runner
                    | ExceptionKind::Value
                    | ExceptionKind::Key,
                ..
            }
        )
    }
    fn description(&self) -> String {
        match self {
            Self::Exception { description, .. } => description.clone(),
            Self::Failed(failure) => described("JobFailed", &failure.reason),
            Self::Stopped { state, .. } => described("Stopped", state),
            Self::Cancelled => t("CancelledError"),
        }
    }
}
fn described(class: &str, message: &str) -> String {
    let stripped = message.trim();
    if stripped.is_empty() {
        return t(class);
    }
    joined(&[class, ": ", stripped])
}
impl From<BuildError> for SessionError {
    fn from(_: BuildError) -> Self {
        Self::internal()
    }
}

/// The waiter may be dropped and resumed without abandoning the owned work.
/// Cancellation requests callback cancellation, including its finally cleanup;
/// the adapter determines filesystem-thread continuation, never this module.
pub trait SessionOperation<T>: Send {
    fn cancel(&self);
    fn settle(&mut self) -> SessionFuture<'_, T>;
}
#[derive(Clone)]
pub struct SessionResult {
    pub job_id: String,
    pub state: String,
    pub detail: Arc<Document>,
    pub step_dirs: Vec<(String, PosixPath)>,
}
impl std::fmt::Debug for SessionResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionResult([redacted])")
    }
}
#[derive(Default)]
pub struct SessionState {
    pub work: Option<PosixPath>,
    pub step_dirs: Vec<(String, PosixPath)>,
    pub uploaded: Option<Arc<Document>>,
}
impl std::fmt::Debug for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionState([redacted])")
    }
}
/// Body callbacks update this ordered session state; the heartbeat only sets lost.
pub type SharedState = Arc<Mutex<SessionState>>;
#[derive(Default)]
struct LostState {
    set: AtomicBool,
    changed: Notify,
}
#[derive(Clone, Default)]
pub struct Lost(Arc<LostState>);
impl Lost {
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.set.load(Ordering::Acquire)
    }
    pub fn set(&self) {
        self.0.set.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
    /// Source Event.wait, latched once the lease is lost; never cancels work.
    pub async fn wait(&self) {
        let changed = self.0.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if !self.is_set() {
            changed.await;
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    Completion,
    Failure,
}
pub struct Post {
    pub endpoint: Endpoint,
    pub body: Arc<Document>,
    pub idempotency_key: Option<String>,
}
impl std::fmt::Debug for Post {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Post")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}
pub trait Response: Send + Sync {
    fn status(&self) -> u16;
    /// Each invocation is a genuine decode; Value errors alone are suppressed
    /// by `error_code`/`_body`. The completion path intentionally decodes twice.
    /// # Errors
    /// Preserves the source decoder's exception class and description.
    fn json(&self) -> Result<Arc<Document>, SessionError>;
    fn error_description(&self, code: Option<&String>) -> String;
}
/// Every effect is required. No real network, work backend, clock, logger, or
/// filesystem cancellation policy is supplied by this source-compatible planner.
pub trait Callbacks: Send + Sync {
    /// Convert the raw heartbeat value before installing run's cleanup scope.
    /// # Errors
    /// Preserves Python float conversion failures.
    fn heartbeat_interval(&self) -> Result<f64, SessionError>;
    fn make_work(&self) -> Box<dyn SessionOperation<PosixPath>>;
    fn body(&self, state: SharedState, lost: Lost) -> Box<dyn SessionOperation<Arc<Document>>>;
    fn sleep(&self, seconds: f64) -> Box<dyn SessionOperation<()>>;
    fn renew(&self, interval: f64) -> Box<dyn SessionOperation<bool>>;
    fn release_job(&self) -> Box<dyn SessionOperation<()>>;
    fn remove_work(&self, path: PosixPath) -> Box<dyn SessionOperation<()>>;
    fn post(&self, request: Post) -> Box<dyn SessionOperation<Arc<dyn Response>>>;
    /// # Errors
    /// Logging failures propagate at their original source position.
    fn log(&self, message: &str) -> Result<(), SessionError>;
    /// Required Python str(value), including non-string state/error-code values.
    /// # Errors
    /// Preserves rendering failures, including recursion and integer limits.
    fn render(&self, document: &Document, node: NodeId) -> Result<String, SessionError>;
}
#[derive(Clone)]
pub struct Input {
    pub job_id: String,
    pub generation: String,
    /// Missing keys fail at completion construction, after body execution.
    pub attempt_id: Option<Arc<Document>>,
    pub keep_dirs: bool,
}
impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Input([redacted])")
    }
}
/// Independent source coroutine boundaries, also available to subclass adapters.
pub enum Action {
    Run,
    Outcome,
    Complete(Arc<Document>),
    Fail(JobFailure),
}
struct StopState {
    epoch: AtomicUsize,
    changed: Notify,
}
#[derive(Clone)]
struct Stop(Arc<StopState>);
impl Stop {
    fn new() -> Self {
        Self(Arc::new(StopState {
            epoch: AtomicUsize::new(0),
            changed: Notify::new(),
        }))
    }
    fn request(&self) {
        self.0.epoch.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
    fn epoch(&self) -> usize {
        self.0.epoch.load(Ordering::Acquire)
    }
    async fn changed(&self, epoch: usize) {
        let notified = self.0.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.epoch() == epoch {
            notified.await;
        }
    }
}
async fn execute<T: Send>(
    mut operation: Box<dyn SessionOperation<T>>,
    stop: &Stop,
    delivered: &mut usize,
) -> Result<T, SessionError> {
    loop {
        let current = stop.epoch();
        if current != *delivered {
            *delivered = current;
            operation.cancel();
        }
        tokio::select! { result=operation.settle()=>return result, ()=stop.changed(*delivered)=>{} }
    }
}
async fn finish(
    mut operation: Box<dyn SessionOperation<()>>,
    stop: &Stop,
) -> Result<(), SessionError> {
    let mut epoch = stop.epoch();
    let mut cancelled = false;
    loop {
        tokio::select! {result=operation.settle()=>return if cancelled || stop.epoch()!=epoch {Err(SessionError::Cancelled)} else {result},()=stop.changed(epoch)=>{cancelled=true;epoch=stop.epoch();}}
    }
}
fn t(value: &str) -> String {
    String::from(value)
}
fn joined(parts: &[&str]) -> String {
    parts.concat()
}
fn document(fields: Vec<(&str, Arc<Document>)>) -> Result<Arc<Document>, SessionError> {
    let mut builder = DocumentBuilder::new();
    let mut items = Vec::new();
    for (key, value) in fields {
        items.push((t(key), builder.import(&value, value.root())?));
    }
    let root = builder.push(Node::Object(items))?;
    Ok(Arc::new(builder.finish(root)?))
}
fn scalar(node: Node) -> Result<Arc<Document>, SessionError> {
    let mut builder = DocumentBuilder::new();
    let root = builder.push(node)?;
    Ok(Arc::new(builder.finish(root)?))
}
fn string(value: String) -> Result<Arc<Document>, SessionError> {
    scalar(Node::String(value))
}
fn state_node(value: &Document) -> Result<NodeId, SessionError> {
    if !matches!(value.node(value.root()), Some(Node::Object(_))) {
        let description = match value.node(value.root()) {
            Some(Node::Null) => "TypeError: 'NoneType' object is not subscriptable",
            Some(Node::Bool(_)) => "TypeError: 'bool' object is not subscriptable",
            Some(Node::Integer(_)) => "TypeError: 'int' object is not subscriptable",
            Some(Node::Float(_)) => "TypeError: 'float' object is not subscriptable",
            Some(Node::String(_)) => "TypeError: string indices must be integers, not 'str'",
            Some(Node::Array(_)) => "TypeError: list indices must be integers or slices, not str",
            _ => "RuntimeError: invalid response arena",
        };
        return Err(SessionError::exception(ExceptionKind::Type, t(description)));
    }
    value
        .field(value.root(), "state")
        .ok_or_else(|| SessionError::exception(ExceptionKind::Key, t("KeyError: 'state'")))
}
#[derive(Clone)]
pub struct Session {
    pub input: Input,
    pub state: SharedState,
    pub lost: Lost,
    callbacks: Arc<dyn Callbacks>,
}
impl Session {
    #[must_use]
    pub fn new(input: Input, callbacks: Arc<dyn Callbacks>) -> Self {
        Self {
            input,
            state: Arc::new(Mutex::new(SessionState::default())),
            lost: Lost::default(),
            callbacks,
        }
    }
    fn result(&self, state: String, detail: Arc<Document>) -> Result<SessionResult, SessionError> {
        Ok(SessionResult {
            job_id: self.input.job_id.clone(),
            state,
            detail,
            step_dirs: self
                .state
                .lock()
                .map_err(|_| SessionError::internal())?
                .step_dirs
                .clone(),
        })
    }
    fn log(&self, parts: &[&String]) -> Result<(), SessionError> {
        self.callbacks.log(&joined(&[
            &t("job "),
            &self.input.job_id,
            &t(": "),
            &parts.iter().map(|part| part.as_str()).collect::<String>(),
        ]))
    }
    fn body(response: &dyn Response) -> Result<Arc<Document>, SessionError> {
        match response.json() {
            Err(SessionError::Exception {
                kind: ExceptionKind::Value,
                ..
            }) => scalar(Node::Integer(response.status().into()))
                .and_then(|status| document(vec![("status", status)])),
            other => other,
        }
    }
    fn error_code(&self, response: &dyn Response) -> Result<Option<String>, SessionError> {
        let value = match response.json() {
            Err(SessionError::Exception {
                kind: ExceptionKind::Value,
                ..
            }) => return Ok(None),
            other => other?,
        };
        let Some(error) = value.field(value.root(), "error") else {
            return Ok(None);
        };
        let Some(code) = value.field(error, "code") else {
            return Ok(None);
        };
        if matches!(value.node(code), Some(Node::Null)) {
            return Ok(None);
        }
        self.callbacks.render(&value, code).map(Some)
    }
    /// Source `_stop_on`, including HTTPX's exact 400..599 error range.
    /// # Errors
    /// Returns Stopped for stale lease/durable 422, otherwise source exceptions.
    pub fn stop_on(&self, response: &dyn Response, durable_422: bool) -> Result<(), SessionError> {
        if !(400..=599).contains(&response.status()) {
            return Ok(());
        }
        let code = self.error_code(response)?;
        if response.status() == 409
            && code
                .as_ref()
                .is_some_and(|code| code.equals_utf8("stale_lease"))
        {
            self.lost.set();
            return Err(SessionError::Stopped {
                state: t("abandoned"),
                detail: Self::body(response)?,
            });
        }
        if response.status() == 422 && durable_422 {
            return Err(SessionError::Stopped {
                state: t("failed"),
                detail: Self::body(response)?,
            });
        }
        Err(SessionError::exception(
            ExceptionKind::Runner,
            response.error_description(code.as_ref()),
        ))
    }
    async fn complete(
        &self,
        evidence: Arc<Document>,
        stop: &Stop,
        delivered: &mut usize,
    ) -> Result<SessionResult, SessionError> {
        let uploaded = self
            .state
            .lock()
            .map_err(|_| SessionError::internal())?
            .uploaded
            .clone();
        let manifest = document(vec![
            ("schema_version", string(t("0.2"))?),
            (
                "attempt_id",
                self.input.attempt_id.clone().ok_or_else(|| {
                    SessionError::exception(ExceptionKind::Key, t("KeyError: 'attempt_id'"))
                })?,
            ),
            (
                "objects",
                match uploaded {
                    Some(value) => value,
                    None => scalar(Node::Array(Vec::new()))?,
                },
            ),
        ])?;
        let body = document(vec![
            ("schema_version", string(t("0.2"))?),
            ("job_id", string(self.input.job_id.clone())?),
            ("evidence", evidence),
            ("manifest", manifest),
        ])?;
        let key = joined(&[
            &t("complete-"),
            &self.input.job_id,
            &t("-"),
            &self.input.generation,
        ]);
        let response = execute(
            self.callbacks.post(Post {
                endpoint: Endpoint::Completion,
                body,
                idempotency_key: Some(key),
            }),
            stop,
            delivered,
        )
        .await?;
        self.stop_on(response.as_ref(), true)?;
        let value = response.json()?;
        let state = self.callbacks.render(&value, state_node(&value)?)?;
        self.result(state, response.json()?)
    }
    async fn fail(
        &self,
        failure: JobFailure,
        stop: &Stop,
        delivered: &mut usize,
    ) -> Result<SessionResult, SessionError> {
        let reports = failure_report::reports(&failure_report::Failure {
            job_id: &self.input.job_id,
            code: &failure.code,
            reason: &failure.reason,
            step: failure.step.as_ref(),
            logs: failure.logs.as_deref(),
        })?
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
        let original = reports
            .first()
            .cloned()
            .ok_or_else(SessionError::internal)?;
        for (index, report) in reports.iter().enumerate() {
            let outcome = async {
                let response = execute(
                    self.callbacks.post(Post {
                        endpoint: Endpoint::Failure,
                        body: report.clone(),
                        idempotency_key: None,
                    }),
                    stop,
                    delivered,
                )
                .await?;
                if response.status() == 422 && index + 1 < reports.len() {
                    self.log(&[&t("failure report rejected; retrying without details")])?;
                    return Ok(None);
                }
                self.stop_on(response.as_ref(), false)?;
                let recorded = response.json()?;
                let state = self.callbacks.render(&recorded, state_node(&recorded)?)?;
                self.result(state, recorded).map(Some)
            }
            .await;
            match outcome {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => {}
                Err(SessionError::Stopped { state, detail }) => return self.result(state, detail),
                Err(error) if error.reportable() => {
                    self.log(&[&t("cannot report its failure: "), &error.description()])?;
                    return self.result(
                        t("unreported"),
                        document(vec![
                            ("error", string(error.description())?),
                            ("report", original),
                        ])?,
                    );
                }
                Err(error) => return Err(error),
            }
        }
        self.result(t("unreported"), document(vec![("report", original)])?)
    }
    async fn outcome(
        &self,
        stop: &Stop,
        delivered: &mut usize,
    ) -> Result<SessionResult, SessionError> {
        let outcome = async {
            let work = execute(self.callbacks.make_work(), stop, delivered).await?;
            self.state
                .lock()
                .map_err(|_| SessionError::internal())?
                .work = Some(work);
            let evidence = execute(
                self.callbacks.body(self.state.clone(), self.lost.clone()),
                stop,
                delivered,
            )
            .await?;
            self.complete(evidence, stop, delivered).await
        }
        .await;
        match outcome {
            Ok(result) => Ok(result),
            Err(SessionError::Failed(failure)) => self.fail(failure, stop, delivered).await,
            Err(SessionError::Stopped { state, detail }) => self.result(state, detail),
            Err(error) if error.ordinary() => {
                self.log(&[&error.description()])?;
                self.fail(
                    JobFailure {
                        code: t("runner_error"),
                        reason: joined(&[
                            &t("the runner could not finish the job: "),
                            &error.description(),
                        ]),
                        step: None,
                        logs: None,
                    },
                    stop,
                    delivered,
                )
                .await
            }
            Err(error) => Err(error),
        }
    }
    async fn heartbeat(&self, interval: f64, stop: &Stop) -> Result<(), SessionError> {
        if stop.epoch() != 0 {
            return Err(SessionError::Cancelled);
        }
        let mut delivered = 0;
        let outcome: Result<(), SessionError> = async {
            loop {
                execute(self.callbacks.sleep(interval), stop, &mut delivered).await?;
                if !execute(self.callbacks.renew(interval), stop, &mut delivered).await? {
                    self.lost.set();
                    return Ok(());
                }
            }
        }
        .await;
        match outcome {
            Err(error) if error.ordinary() => {
                self.log(&[&t("heartbeat stopped: "), &error.description()])?;
                self.lost.set();
                Ok(())
            }
            other => other,
        }
    }
    async fn run(&self, stop: &Stop) -> Result<SessionResult, SessionError> {
        let interval = self.callbacks.heartbeat_interval()?;
        let beat_stop = Stop::new();
        let beat_session = self.clone();
        let own_stop = beat_stop.clone();
        let mut beat =
            tokio::spawn(async move { beat_session.heartbeat(interval, &own_stop).await });
        let mut delivered = 0;
        let mut result = self.outcome(stop, &mut delivered).await;
        beat_stop.request();
        loop {
            let epoch = stop.epoch();
            if epoch != delivered {
                delivered = epoch;
                beat_stop.request();
            }
            tokio::select! {
                settled = &mut beat => {
                    match settled {
                        Ok(Ok(()) | Err(SessionError::Cancelled)) => {},
                        Ok(Err(error)) => result = Err(error),
                        Err(_) => result = Err(SessionError::internal()),
                    }
                    break;
                },
                () = stop.changed(delivered) => {},
            }
        }
        finish(self.callbacks.release_job(), stop).await?;
        if !self.input.keep_dirs {
            let work = self
                .state
                .lock()
                .map_err(|_| SessionError::internal())?
                .work
                .clone();
            if let Some(work) = work {
                execute(self.callbacks.remove_work(work), stop, &mut delivered).await?;
            }
        }
        result
    }
    /// Start an owned task, preserving callback cleanup across waiter cancellation.
    #[must_use]
    pub fn start(self) -> SessionTask<SessionResult> {
        self.start_action(Action::Run)
    }
    /// Start one coroutine through the same required operation boundaries.
    #[must_use]
    pub fn start_action(self, action: Action) -> SessionTask<SessionResult> {
        let stop = Stop::new();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            if task_stop.epoch() != 0 {
                return Err(SessionError::Cancelled);
            }
            let mut delivered = 0;
            match action {
                Action::Run => self.run(&task_stop).await,
                Action::Outcome => self.outcome(&task_stop, &mut delivered).await,
                Action::Complete(evidence) => {
                    self.complete(evidence, &task_stop, &mut delivered).await
                }
                Action::Fail(failure) => self.fail(failure, &task_stop, &mut delivered).await,
            }
        });
        SessionTask {
            stop,
            task: Some(task),
            result: None,
        }
    }
    /// Start the independent heartbeat coroutine, whose source result is unit.
    #[must_use]
    pub fn start_heartbeat(self, interval: f64) -> SessionTask<()> {
        let stop = Stop::new();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            if task_stop.epoch() != 0 {
                return Err(SessionError::Cancelled);
            }
            self.heartbeat(interval, &task_stop).await
        });
        SessionTask {
            stop,
            task: Some(task),
            result: None,
        }
    }
}
pub struct SessionTask<T: Clone> {
    stop: Stop,
    task: Option<tokio::task::JoinHandle<Result<T, SessionError>>>,
    result: Option<Result<T, SessionError>>,
}
impl<T: Clone> SessionTask<T> {
    pub fn cancel(&self) {
        self.stop.request();
    }
    /// # Errors
    /// Returns the source outcome/cleanup exception after owned settlement.
    pub async fn settle(&mut self) -> Result<T, SessionError> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let result = match self.task.as_mut() {
            Some(task) => task.await.unwrap_or_else(|_| Err(SessionError::internal())),
            None => return Err(SessionError::internal()),
        };
        self.task.take();
        self.result = Some(result.clone());
        result
    }
}
impl<T: Clone> Drop for SessionTask<T> {
    fn drop(&mut self) {
        self.stop.request();
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
