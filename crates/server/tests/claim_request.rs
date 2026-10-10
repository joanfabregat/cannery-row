//! Authored claim body contracts and retained principal mode coverage.
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    ids::{ProjectId, ServiceAccountId, UserId},
    json,
    principal::{Channel, Principal, ServiceKind, ServicePrincipal, UserPrincipal, Via},
};
use cannery_server::{
    claim_request::{ClaimRequest, claim_mode},
    validation::BodyInput,
};
use cannery_tracks::repo::TrackMode;
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn fixture() -> Result<Value, serde_json::Error> {
    serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/claim_request_reference.json"
    ))
}
#[test]
fn claim_body_requires_an_object_with_optional_strict_fields() -> TestResult {
    let document = json::decode_str(r#"{"unit":17,"track":"trial","mode":"workflow"}"#, 64)?;
    let actual = ClaimRequest::parse(BodyInput::Json(&document))?;
    assert_eq!(actual.unit, Some(17.into()));
    assert_eq!(actual.track.as_deref(), Some("trial"));
    assert_eq!(actual.mode, Some(TrackMode::Workflow));
    assert_eq!(
        actual.fields_set,
        BTreeSet::from(["unit".into(), "track".into(), "mode".into()])
    );
    assert_eq!(format!("{actual:?}"), "ClaimRequest([redacted])");

    for (raw, present) in [
        ("{}", false),
        (r#"{"unit":null,"track":null,"mode":null}"#, true),
    ] {
        let document = json::decode_str(raw, 64)?;
        let actual = ClaimRequest::parse(BodyInput::Json(&document))?;
        assert!(actual.unit.is_none());
        assert!(actual.track.is_none());
        assert!(actual.mode.is_none());
        assert_eq!(actual.fields_set.len(), if present { 3 } else { 0 });
    }
    for unit in [1, i32::MAX] {
        let document = json::decode_str(&format!(r#"{{"unit":{unit}}}"#), 64)?;
        assert_eq!(
            ClaimRequest::parse(BodyInput::Json(&document))?.unit,
            Some(unit.into())
        );
    }
    for unit in [
        r#""17""#,
        "17.0",
        "true",
        "false",
        "0",
        "-1",
        "2147483648",
        "9223372036854775808",
        "[]",
        "{}",
    ] {
        let document = json::decode_str(&format!(r#"{{"unit":{unit}}}"#), 64)?;
        assert!(
            ClaimRequest::parse(BodyInput::Json(&document)).is_err(),
            "unit {unit}"
        );
    }
    assert!(ClaimRequest::parse(BodyInput::Missing).is_err());
    assert!(ClaimRequest::parse(BodyInput::RawBytes).is_err());
    for raw in [
        "null",
        "[]",
        "true",
        "1",
        r#""text""#,
        r#"{"unknown":true}"#,
        r#"{"track":17}"#,
        r#"{"mode":"unknown"}"#,
    ] {
        let document = json::decode_str(raw, 64)?;
        assert!(
            ClaimRequest::parse(BodyInput::Json(&document)).is_err(),
            "{raw}"
        );
    }
    Ok(())
}
fn principal(kind: &str) -> Principal {
    let via = Via {
        channel: Channel::Api,
        client: None,
    };
    if kind == "user" || kind == "admin" {
        Principal::User(UserPrincipal {
            user_id: UserId(Uuid::from_u128(1)),
            email: None,
            display_name: None,
            is_admin: kind == "admin",
            via,
            scopes: BTreeSet::new(),
            session_id: None,
            csrf_token: None,
        })
    } else {
        let kind = match kind {
            "agent" => ServiceKind::Agent,
            "experimenter" => ServiceKind::Experimenter,
            _ => ServiceKind::Verifier,
        };
        Principal::Service(ServicePrincipal {
            service_account_id: ServiceAccountId(Uuid::from_u128(2)),
            project_id: ProjectId(Uuid::from_u128(3)),
            kind,
            name: "fixture".into(),
            via,
            scopes: BTreeSet::new(),
        })
    }
}
#[test]
fn claim_mode_matches_unchanged_principal_helper() -> TestResult {
    let fixture = fixture()?;
    let cases = fixture["principals"].as_array().ok_or("principals")?;
    for case in cases {
        let principal = principal(case["principal"].as_str().ok_or("principal")?);
        let requested = case["requested"]
            .as_str()
            .map(TrackMode::try_from)
            .transpose()?;
        let observed = match claim_mode(&principal, requested) {
            Ok(mode) => serde_json::json!({"mode":mode.as_str()}),
            Err(error) => {
                serde_json::json!({"code":error.code,"message":error.message,"details":error.details,"status":error.code.status()})
            }
        };
        assert_eq!(
            observed, case["outcome"],
            "{} / {}",
            case["principal"], case["requested"]
        );
    }
    assert_eq!(cases.len(), 18);
    Ok(())
}
