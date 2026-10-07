//! Actual Python scheduler references plus owned-operation cancellation witnesses.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    cancellation::CancellationEvent,
    worker_process::{
        Entry, ErrorKind, JobResult, LogSink, Operation, ProcessError, ProcessFuture, ProcessTask,
        Request, SharedLifecycle, Stream, Wait, Worker, WorkerFactory,
    },
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

fn text(s: &str) -> String {
    String::from(s)
}
fn error(description: &str) -> ProcessError {
    ProcessError::new(ErrorKind::Unexpected, text(description))
}
struct TestOperation<T: Send + 'static> {
    future: Option<ProcessFuture<'static, T>>,
    task: Option<tokio::task::JoinHandle<Result<T, ProcessError>>>,
    cancellation: CancellationEvent,
}
impl<T: Send + 'static> Operation<T> for TestOperation<T> {
    fn cancel(&self) {
        self.cancellation.set();
    }
    fn settle(&mut self) -> ProcessFuture<'_, T> {
        if let Some(future) = self.future.take() {
            self.task = Some(tokio::spawn(future));
        }
        Box::pin(async move {
            self.task
                .as_mut()
                .expect("owned task")
                .await
                .expect("test task")
        })
    }
}
impl<T: Send + 'static> Drop for TestOperation<T> {
    fn drop(&mut self) {
        self.cancellation.set();
    }
}
fn operation<T: Send + 'static, F: Future<Output = Result<T, ProcessError>> + Send + 'static>(
    cancellation: CancellationEvent,
    future: F,
) -> Box<dyn Operation<T>> {
    Box::new(TestOperation {
        future: Some(Box::pin(future)),
        task: None,
        cancellation,
    })
}
#[derive(Default)]
struct Facts {
    trace: Vec<String>,
    runs: Vec<usize>,
    cleaned: Vec<usize>,
    prefixes: Vec<String>,
    stdout: Vec<String>,
    stderr: Vec<String>,
    waits: Vec<f64>,
    parent_repeated_ignored: bool,
    completions: Vec<usize>,
    flushes: [usize; 2],
}
struct Fixture {
    recipe: Value,
    facts: Mutex<Facts>,
    ready: CancellationEvent,
    closing: CancellationEvent,
    release: CancellationEvent,
    all_closed: CancellationEvent,
    continued: CancellationEvent,
    poll_cancellation: Mutex<Option<CancellationEvent>>,
}
impl Fixture {
    fn flag(&self, name: &str, default: bool) -> bool {
        self.recipe[name].as_bool().unwrap_or(default)
    }
    fn behaviors(&self) -> &Vec<Value> {
        self.recipe["behaviors"].as_array().unwrap()
    }
    fn concurrency(&self) -> usize {
        self.recipe["concurrency"]
            .as_u64()
            .unwrap_or(1)
            .try_into()
            .unwrap()
    }
    fn effect(&self, stage: &str) -> Result<(), ProcessError> {
        self.facts.lock().unwrap().trace.push(stage.to_owned());
        if self.recipe["fail"]
            .as_array()
            .is_some_and(|v| v.iter().any(|s| s == stage))
        {
            Err(error(&format!("FixtureError: {stage}")))
        } else {
            Ok(())
        }
    }
    fn effect_op(self: &Arc<Self>, stage: &str) -> Box<dyn Operation<()>> {
        let own = self.clone();
        let stage = stage.to_owned();
        let signal = CancellationEvent::new();
        let cancellation = signal.clone();
        operation(cancellation, async move {
            let result = own.effect(&stage);
            if stage == "launcher-start" && own.flag("start_catches_cancel", false) {
                own.ready.set();
                signal.wait().await;
            }
            if stage == "client-close" && own.recipe["cancel_client_close"].is_string() {
                own.closing.set();
                signal.wait().await;
                match own.recipe["cancel_client_close"].as_str().unwrap() {
                    "propagates" => return Err(ProcessError::cancelled()),
                    "raises" => return Err(error("FixtureError: client-cancel-cleanup")),
                    _ => {}
                }
            }
            if stage == "launcher-close"
                || (!own.flag("has_launcher", true) && stage == "client-close")
            {
                own.all_closed.set();
            }
            result
        })
    }
}
struct TestWorker {
    fixture: Arc<Fixture>,
    index: usize,
}
impl Worker for TestWorker {
    fn run_once(&self) -> Result<Box<dyn Operation<Option<JobResult>>>, ProcessError> {
        let s = self.fixture.clone();
        let i = self.index;
        let cancel = CancellationEvent::new();
        let signal = cancel.clone();
        Ok(operation(cancel, async move {
            let result = async {
                let b = s.behaviors()[i].as_str().unwrap().to_owned();
                let runs = {
                    let mut facts = s.facts.lock().unwrap();
                    facts.runs[i] += 1;
                    facts.runs[i]
                };
                if s.flag("ordered_completion", false) {
                    if i == 0 {
                        s.release.wait().await;
                    } else {
                        s.release.set();
                    }
                }
                if b == "hang" || (b.ends_with("-then-hang") && runs == 2) {
                    let target = if b == "hang" { s.concurrency() } else { 2 };
                    if s.facts.lock().unwrap().runs.iter().sum::<usize>()
                        == target * s.behaviors().len()
                    {
                        s.ready.set();
                    }
                    signal.wait().await;
                    tokio::task::yield_now().await;
                    s.facts.lock().unwrap().cleaned[i] += 1;
                    if s.flag("cancel_cleanup_error", false) {
                        return Err(error("FixtureError: cancel-cleanup"));
                    }
                    return Err(ProcessError::cancelled());
                }
                match b.as_str() {
                    b if b.starts_with("result") || b == "blocked-result" => Ok(Some(JobResult {
                        job_id: text(&format!("job-{i}")),
                        state: text("completed"),
                    })),
                    b if b.starts_with("expected") => {
                        let display = s.recipe["message"].as_str().unwrap_or("refused");
                        let trimmed = display.trim();
                        let description = if trimmed.is_empty() {
                            "RunnerError".to_owned()
                        } else {
                            format!("RunnerError: {trimmed}")
                        };
                        Err(ProcessError::expected(text(display), text(&description)))
                    }
                    b if b.starts_with("unexpected") => Err(error("RuntimeError: broken")),
                    "fatal" | "blocked-fatal" => Err(ProcessError::new(
                        ErrorKind::Fatal,
                        text(&format!("FixtureFatal: fatal-{i}")),
                    )),
                    "cancelled" => Err(ProcessError::cancelled()),
                    b if b.starts_with("ordinary-group") => Err(ProcessError {
                        kind: ErrorKind::Group,
                        display: text("nested (1 sub-exception)"),
                        description: text("ExceptionGroup: nested (1 sub-exception)"),
                        causes: vec![error("RuntimeError: broken")],
                    }),
                    "base-group" => Err(ProcessError {
                        kind: ErrorKind::BaseGroup,
                        display: text("nested (1 sub-exception)"),
                        description: text("BaseExceptionGroup: nested (1 sub-exception)"),
                        causes: vec![ProcessError::new(
                            ErrorKind::Fatal,
                            text("FixtureFatal: fatal"),
                        )],
                    }),
                    _ => Ok(None),
                }
            }
            .await;
            if s.flag("ordered_completion", false) {
                s.facts.lock().unwrap().completions.push(i);
            }
            result
        }))
    }
    fn aclose(&self) -> Box<dyn Operation<()>> {
        let s = self.fixture.clone();
        let i = self.index;
        operation(CancellationEvent::new(), async move {
            if s.flag("repeat_cancel", false) {
                s.closing.set();
                s.release.wait().await;
            }
            s.effect(&format!("worker-close-{i}"))
        })
    }
}
struct Boundary(Arc<Fixture>);
impl std::ops::Deref for Boundary {
    type Target = Arc<Fixture>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl WorkerFactory for Boundary {
    fn create(
        &self,
        index: usize,
        _: &Entry,
        prefix: &str,
    ) -> Result<Arc<dyn Worker>, ProcessError> {
        self.effect(&format!("factory-{index}"))?;
        self.facts.lock().unwrap().prefixes.push(prefix.to_owned());
        Ok(Arc::new(TestWorker {
            fixture: self.0.clone(),
            index,
        }))
    }
}
impl SharedLifecycle for Boundary {
    fn launcher_start(&self) -> Box<dyn Operation<()>> {
        self.effect_op("launcher-start")
    }
    fn launcher_close(&self) -> Box<dyn Operation<()>> {
        self.effect_op("launcher-close")
    }
    fn client_open(&self) -> Box<dyn Operation<()>> {
        self.effect_op("client-open")
    }
    fn client_close(&self) -> Box<dyn Operation<()>> {
        self.effect_op("client-close")
    }
    fn code_close(&self) -> Box<dyn Operation<()>> {
        self.effect_op("code-close")
    }
}
impl Wait for Boundary {
    fn sleep(&self, seconds: f64) -> Box<dyn Operation<()>> {
        let own = self.0.clone();
        let cancel = CancellationEvent::new();
        let signal = cancel.clone();
        *own.poll_cancellation.lock().unwrap() = Some(signal.clone());
        operation(cancel, async move {
            own.facts.lock().unwrap().waits.push(seconds);
            if own.flag("cancel_cleanup_error", false) {
                own.continued.set();
            }
            if own.flag("fail_wait", false) {
                return Err(error("FixtureError: wait"));
            }
            tokio::select! {
                ()=signal.wait()=>Err(ProcessError::cancelled()),
                ()=async {if seconds==0.0 {tokio::task::yield_now().await;} else {tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;}}=>Ok(())
            }
        })
    }
}
impl LogSink for Boundary {
    fn line(&self, stream: Stream, value: &str) -> Result<(), ProcessError> {
        if self.flag("fail_output", false) {
            return Err(error("FixtureError: output"));
        }
        let mut facts = self.facts.lock().unwrap();
        match stream {
            Stream::Stdout => &mut facts.stdout,
            Stream::Stderr => &mut facts.stderr,
        }
        .push(value.to_owned());
        if stream == Stream::Stderr {
            facts.flushes[1] += 1;
            if self.flag("fail_flush", false) {
                return Err(error("FixtureError: flush"));
            }
        }
        Ok(())
    }
}
fn request(s: &Arc<Fixture>) -> Request {
    let boundary = Arc::new(Boundary(s.clone()));
    Request {
        entries: s
            .behaviors()
            .iter()
            .enumerate()
            .map(|(i, _)| Entry {
                name: text(&format!("entry-{i}")),
                poll_seconds: s.recipe["poll_seconds"].as_f64().unwrap_or(0.001),
                concurrency: BigInt::from(s.concurrency()),
            })
            .collect(),
        prog: text("p"),
        once: s.flag("once", true),
        has_launcher: s.flag("has_launcher", true),
        external_client: s.flag("external_client", false),
        factory: boundary.clone(),
        lifecycle: boundary.clone(),
        wait: boundary.clone(),
        output: boundary,
    }
}
fn fixture(recipe: &Value) -> Arc<Fixture> {
    let n = recipe["behaviors"].as_array().unwrap().len();
    Arc::new(Fixture {
        recipe: recipe.clone(),
        facts: Mutex::new(Facts {
            runs: vec![0; n],
            cleaned: vec![0; n],
            ..Facts::default()
        }),
        ready: CancellationEvent::new(),
        closing: CancellationEvent::new(),
        release: CancellationEvent::new(),
        all_closed: CancellationEvent::new(),
        continued: CancellationEvent::new(),
        poll_cancellation: Mutex::new(None),
    })
}
fn failure(e: &ProcessError) -> Value {
    if matches!(e.kind, ErrorKind::Group | ErrorKind::BaseGroup) {
        json!({"kind":"Group","causes":e.causes.iter().map(failure).collect::<Vec<_>>()})
    } else {
        json!({"kind":format!("{:?}",e.kind),"description":e.description.as_utf8().unwrap()})
    }
}
async fn observe(recipe: &Value) -> Value {
    let s = fixture(recipe);
    let task = ProcessTask::start(request(&s));
    if s.flag("cancel_before_start", false) {
        task.cancel();
    }
    if s.flag("start_catches_cancel", false) {
        s.ready.wait().await;
        task.cancel();
    }
    if s.recipe["cancel_client_close"].is_string() {
        s.closing.wait().await;
        task.cancel();
    }
    if s.behaviors()
        .iter()
        .any(|b| b.as_str().unwrap().contains("hang"))
    {
        s.ready.wait().await;
        task.cancel();
        if s.flag("cancel_cleanup_error", false) {
            s.continued.wait().await;
            task.cancel();
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            let child = s
                .poll_cancellation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            s.facts.lock().unwrap().parent_repeated_ignored =
                !child.is_set() && !s.all_closed.is_set();
            child.set();
        }
        if s.flag("repeat_cancel", false) {
            s.closing.wait().await;
            task.cancel();
            tokio::task::yield_now().await;
            s.release.set();
        }
    }
    let outcome = match task.settle().await {
        Ok(status) => json!({"status":status}),
        Err(e) => json!({"error":failure(&e)}),
    };
    let facts = s.facts.lock().unwrap();
    let output = |lines: &Vec<String>| {
        lines.iter().fold(String::new(), |mut out, line| {
            out.push_str(line);
            out.push('\n');
            out
        })
    };
    json!({"recipe":recipe,"outcome":outcome,"trace":facts.trace,"runs":facts.runs,"cleaned":facts.cleaned,"prefixes":facts.prefixes,"stdout":output(&facts.stdout),"stderr":output(&facts.stderr),"waits":facts.waits,"parent_repeated_ignored":facts.parent_repeated_ignored,"completions":facts.completions,"flushes":facts.flushes})
}
#[tokio::test]
async fn worker_process_actual_python_reference() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/worker_process_reference.json"
    ))
    .unwrap();
    assert_eq!(reference["python"], "3.13.11");
    assert_eq!(reference["unicode"], "15.1.0");
    assert_eq!(reference["case_count"], 55);
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 55);
    assert_eq!(
        reference["section_counts"],
        json!({"once":39,"continuous":16})
    );
    assert_eq!(
        cases
            .iter()
            .filter(|case| case["recipe"]["once"].as_bool().unwrap_or(true))
            .count(),
        39
    );
    for expected in cases {
        let mut expected = expected.clone();
        // Private asyncio unretrieved-task warning differs from owned-op error
        // consumption. Its source observation is retained, not a public result.
        expected
            .as_object_mut()
            .unwrap()
            .remove("source_only_unretrieved_cleanup");
        let actual = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            observe(&expected["recipe"]),
        )
        .await
        .expect("controlled callback settles");
        assert_eq!(actual, expected, "{}", expected["recipe"]["name"]);
    }
}
#[tokio::test]
async fn dropping_supervisor_waiter_still_settles_all_owned_cleanup() {
    let s = fixture(&json!({"name":"abandoned","behaviors":["hang"],"once":false}));
    let task = ProcessTask::start(request(&s));
    s.ready.wait().await;
    // Poll settle once, then abandon the waiter while run_once remains owned.
    let mut waiter: Pin<Box<dyn Future<Output = Result<i32, ProcessError>>>> =
        Box::pin(task.settle());
    tokio::select! {_result=&mut waiter=>panic!("blocked operation unexpectedly returned"), ()=tokio::task::yield_now()=>{}}
    drop(waiter);
    tokio::time::timeout(std::time::Duration::from_secs(5), s.all_closed.wait())
        .await
        .unwrap();
    let facts = s.facts.lock().unwrap();
    assert_eq!(facts.cleaned, vec![1]);
    assert_eq!(
        facts.trace,
        vec![
            "launcher-start",
            "client-open",
            "factory-0",
            "worker-close-0",
            "code-close",
            "client-close",
            "launcher-close"
        ]
    );
}
#[test]
fn worker_process_debug_omits_callback_descriptions() {
    let error = ProcessError::new(ErrorKind::Unexpected, text("secret callback value"));
    assert!(!format!("{error:?}").contains("secret"));
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
