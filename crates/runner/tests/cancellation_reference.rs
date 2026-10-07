//! Replay actual asyncio.Event scheduling observations without wall-clock waits.
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_runner::cancellation::CancellationEvent;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

struct Waiter<'a> {
    future: Option<Pin<Box<dyn Future<Output = ()> + Send + 'a>>>,
    done: bool,
}

#[test]
fn all_source_event_observations_match() -> Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/cancellation_reference.json"
    ))?;
    assert_eq!(fixture["python"], "3.13.11");
    let cases = fixture["cases"].as_array().ok_or("missing cases")?;
    assert_eq!(cases.len(), 347);
    for case in cases {
        let event = CancellationEvent::new();
        let mut waiters = Vec::new();
        let mut observations = Vec::new();
        for operation in case["operations"].as_array().ok_or("missing operations")? {
            match operation.as_str().ok_or("missing operation")? {
                "set" => event.clone().set(),
                "clear" => event.clone().clear(),
                "wait" => waiters.push(Waiter {
                    future: Some(Box::pin(event.wait())),
                    done: false,
                }),
                "tick" => {
                    let mut cx = Context::from_waker(Waker::noop());
                    for waiter in &mut waiters {
                        if let Some(future) = &mut waiter.future
                            && future.as_mut().poll(&mut cx) == Poll::Ready(())
                        {
                            waiter.future = None;
                            waiter.done = true;
                        }
                    }
                }
                _ => return Err("unknown operation".into()),
            }
            observations.push(json!({
                "set": event.is_set(),
                "waiters": waiters.iter().map(|waiter| waiter.done.then_some(true)).collect::<Vec<_>>()
            }));
        }
        assert_eq!(
            json!(observations),
            case["observations"],
            "case {}",
            case["name"]
        );
    }
    Ok(())
}

#[test]
fn dropping_one_waiter_does_not_consume_a_broadcast() {
    let event = CancellationEvent::new();
    let mut first = Box::pin(event.wait());
    let mut second = Box::pin(event.wait());
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(first.as_mut().poll(&mut cx), Poll::Pending);
    assert_eq!(second.as_mut().poll(&mut cx), Poll::Pending);
    drop(first);
    event.clone().set();
    event.clear();
    assert_eq!(second.as_mut().poll(&mut cx), Poll::Ready(()));
    let mut later = Box::pin(event.wait());
    assert_eq!(later.as_mut().poll(&mut cx), Poll::Pending);
}
