//! Actual production renewal traces replayed across the required adapter boundaries.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_runner::lease_retry::{Decision, Renewal, Reply};
use serde_json::{Value, json};

fn float(value: &Value) -> f64 {
    f64::from_bits(u64::from_str_radix(value.as_str().unwrap(), 16).unwrap())
}

fn bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

#[test]
fn production_renewal_retry_order_and_float_bits() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/tests/fixtures/lease_retry_reference.json"
    ))
    .unwrap();
    assert_eq!(reference["format"], 1);
    assert_eq!(reference["python"], "3.13.11");
    assert_eq!(reference["count"], 259);
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 259);
    for (index, case) in cases.iter().enumerate() {
        let mut expires = float(&case["expires"]);
        let now = float(&case["now"]);
        let mut renewal = Renewal::new(float(&case["interval"]), expires);
        let mut trace = Vec::new();
        let mut result = None;
        for answer in case["answers"].as_array().unwrap() {
            let answer = answer.as_str().unwrap();
            trace.push(json!({"request":answer}));
            let reply = match answer {
                "success" => {
                    trace.push(json!({"expiry":"2030-01-01T00:00:00Z"}));
                    Reply::Renewed { expires_at: 5000.0 }
                }
                "stale" => Reply::StaleLease,
                _ => Reply::Transient,
            };
            match renewal.response(reply, || {
                trace.push(json!({"clock":bits(now)}));
                now
            }) {
                Decision::Renewed { expires_at } => {
                    expires = expires_at;
                    result = Some(true);
                    break;
                }
                Decision::Lost => {
                    trace.push(json!({"log":"job fixture: lease lost"}));
                    result = Some(false);
                    break;
                }
                decision => {
                    let problem = if answer == "transport" {
                        "ConnectError: fixture".to_owned()
                    } else {
                        format!("HTTP {answer}")
                    };
                    match decision {
                        Decision::Expired => {
                            trace.push(json!({"log":format!(
                                "job fixture: lease expired while the heartbeat failed ({problem})"
                            )}));
                            result = Some(false);
                            break;
                        }
                        Decision::Sleep { seconds } => {
                            trace.push(json!({"log":format!(
                                "job fixture: heartbeat failed ({problem}); retrying"
                            )}));
                            trace.push(json!({"sleep":bits(seconds)}));
                            renewal.slept();
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
        let result = result.expect("recipe must finish through a source decision");
        assert_eq!(
            json!({"result":result,"expires":bits(expires),"trace":trace}),
            case["expected"],
            "source recipe {index}"
        );
    }
}
