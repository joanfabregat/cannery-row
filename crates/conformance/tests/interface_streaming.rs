#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Shared lifecycle helpers also serve broader scenario suites"
)]
mod lifecycle;
#[path = "support/interface_streaming_support.rs"]
mod support;
use conformance::Result;
use serde_json::{Value, json};
use std::io::Write;
use support::{grant, prepared, put, registration};

fn row() -> Value {
    json!({"type":"object","required":["query","score"],"properties":{"query":{"type":"string"},"score":{"type":"number"}}})
}
fn run() -> Value {
    json!({"type":"object","required":["queries"],"properties":{"queries":{"type":"array","items":{"type":"object","properties":{"docs":{"type":"array","items":{"type":"object","properties":{"score":{"type":"number"}}}}}}}}})
}
struct Case {
    name: &'static str,
    interface: &'static str,
    bytes: Vec<u8>,
    issues: Value,
}
fn pass(name: &'static str, interface: &'static str, bytes: &[u8]) -> Case {
    Case {
        name,
        interface,
        bytes: bytes.to_vec(),
        issues: json!([]),
    }
}
fn fail(name: &'static str, interface: &'static str, bytes: &[u8], message: &str) -> Case {
    let detail = if let Some((text, suffix)) = message.rsplit_once(" (line ") {
        let line = suffix.trim_end_matches(')').parse::<usize>();
        line.map_or_else(
            |_| json!({"message":message}),
            |line| json!({"message":text,"line":line}),
        )
    } else {
        json!({"message":message})
    };
    Case {
        name,
        interface,
        bytes: bytes.to_vec(),
        issues: json!([detail]),
    }
}

/// Python reference prose documents cases; parity checks only structured issue fields.
fn assert_issues(actual: &Value, expected: &Value) -> Result<()> {
    fn structure(issues: &Value) -> Result<Vec<String>> {
        let mut output = Vec::new();
        for issue in issues.as_array().ok_or("issue array missing")? {
            let message = issue["message"].as_str().ok_or("issue message missing")?;
            assert_ne!(message.trim(), "");
            assert!(!message.contains("SECRET-7f3a"));
            let mut fields = issue.as_object().ok_or("issue object missing")?.clone();
            fields.remove("message");
            output.push(serde_json::to_string(&fields)?);
        }
        output.sort();
        Ok(output)
    }
    assert_eq!(structure(actual)?, structure(expected)?);
    Ok(())
}

#[tokio::test]
#[ignore = "requires ordinary conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "The table preserves individual interface contract examples and their exact issue details"
)]
async fn interface_registered_content_matrix() -> Result<()> {
    let interfaces = vec![
        registration("probe-row", json!({"schema":row()})),
        registration("probe-run", json!({"schema":run()})),
        registration("probe-jsonl", json!({"schema":row(),"encoding":"jsonl"})),
        registration("probe-empty", json!({"schema":row(),"allow_empty":true})),
        registration(
            "probe-size",
            json!({"schema":row(),"encoding":"jsonl","max_bytes":32}),
        ),
        registration(
            "probe-gzip",
            json!({"format":"run","media_type":"application/gzip"}),
        ),
        registration(
            "probe-zip",
            json!({"format":"bundle","media_type":"application/zip"}),
        ),
        registration(
            "probe-parquet",
            json!({"format":"parquet","media_type":"application/vnd.apache.parquet"}),
        ),
        registration(
            "probe-hex",
            json!({"format":"raw","media_type":"application/octet-stream","magic":"cafe"}),
        ),
        registration(
            "probe-custom",
            json!({"format":"custom","media_type":"application/octet-stream"}),
        ),
        registration(
            "probe-geo",
            json!({"format":"geo","media_type":"application/geo+json"}),
        ),
        registration(
            "probe-ndjson",
            json!({"format":"ndjson","media_type":"application/x-ndjson"}),
        ),
        registration("probe-unchecked", json!({"schema":run(),"validate":false})),
        registration(
            "probe-private",
            json!({"schema":{"type":"object","required":["id","label","kind"],"additionalProperties":false,"properties":{"id":{"type":"string","pattern":"^[a-z]+$"},"label":{"type":"string","maxLength":3},"kind":{"enum":["a","b"]},"rank":{"type":"integer","minimum":1}}}}),
        ),
    ];
    let (mut world, actors, job) = prepared("interface-matrix", interfaces).await?;
    let valid = b"{\"query\":\"q\",\"score\":1}";
    let mut cases=vec![
        pass("schema-default","probe-row",valid),
        fail("empty","probe-row",b"","is empty, and the interface does not allow empty files"),
        pass("allowed-empty","probe-empty",b""),
        fail("size","probe-size",&[valid.as_slice(),b"\n",valid.as_slice(),b"\n"].concat(),"is larger than the interface's max_bytes (32 bytes)"),
        fail("gzip-wrong","probe-gzip",b"plain text","does not start like gzip (starts with 706c61696e207465)"),
        pass("gzip-header","probe-gzip",b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03"),
        fail("zip-short","probe-zip",b"PK","does not start like zip (starts with 504b)"),
        pass("zip-header","probe-zip",b"PK\x05\x06\x00\x00\x00\x00"),
        pass("parquet-header","probe-parquet",b"PAR1 anything goes"),
        fail("parquet-wrong","probe-parquet",b"wrong","does not start like parquet (starts with 77726f6e67)"),
        pass("hex-header","probe-hex",b"\xca\xfehello"),
        fail("hex-wrong","probe-hex",b"\xca\xfb","does not start like the bytes cafe (starts with cafb)"),
        pass("custom-default","probe-custom",b"arbitrary content"),
        pass("geo-default","probe-geo",valid),
        pass("ndjson-default","probe-ndjson",&[valid.as_slice(),b"\n",valid.as_slice()].concat()),
        fail("json-magic","probe-run",b"  <html>","does not start with a JSON value"),
        fail("json-parse","probe-run",b"{\"queries\": [","is not valid JSON: Expecting value at column 14"),
        pass("unchecked-parse","probe-unchecked",b"{\"queries\": \"not an array\""),
        fail("unchecked-empty","probe-unchecked",b"","is empty, and the interface does not allow empty files"),
        fail("unchecked-magic","probe-unchecked",b"GIF89a","does not start with a JSON value"),
        pass("utf8-json","probe-row","{\"query\":\"café 日本語 🦀\",\"score\":1}".as_bytes()),
        pass("utf8-jsonl","probe-jsonl","{\"query\":\"café 日本語 🦀\",\"score\":1}\n".as_bytes()),
        pass("bom-json","probe-row",&[b"\xef\xbb\xbf".as_slice(),valid.as_slice()].concat()),
        pass("bom-jsonl","probe-jsonl",&[b"\xef\xbb\xbf".as_slice(),valid.as_slice(),b"\n",valid.as_slice()].concat()),
        fail("bom-later","probe-jsonl",&[valid.as_slice(),b"\n\xef\xbb\xbf",valid.as_slice(),b"\n"].concat(),"is not valid JSON: Unexpected UTF-8 BOM (decode using utf-8-sig) at column 1 (line 2)"),
        fail("utf8-invalid","probe-row",b"{\"query\": \"caf\xe9\", \"score\": 1}","is not valid UTF-8 (at byte 15)"),
        fail("utf8-invalid-line","probe-jsonl",&[valid.as_slice(),b"\n{\"query\": \"caf\xe9\", \"score\": 1}\n"].concat(),"is not valid UTF-8 (at byte 15) (line 2)"),
        fail("jsonl-blank-parse","probe-jsonl",b"{\"query\": \"a\", \"score\": 1}\r\n\n{\"query\": \"b\", \"score\": 2\n{\"query\": \"c\", \"score\": 3}","is not valid JSON: Expecting ',' delimiter at column 26 (line 3)"),
    ];
    cases.push(Case {
        name: "pointer",
        interface: "probe-run",
        bytes: serde_json::to_vec(
            &json!({"queries":[{"docs":[{"score":1}]},{"docs":[{"score":"high"}]}]}),
        )?,
        issues: json!([{"message":"must be of type number","path":"/queries/1/docs/0/score"}]),
    });
    let mut rows = (0..10)
        .map(|i| json!({"query":format!("q{i}"),"score":i}))
        .collect::<Vec<_>>();
    rows[6]["score"] = json!("x");
    let mut row_bytes = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut row_bytes, row)?;
        row_bytes.push(b'\n');
    }
    cases.push(Case {
        name: "jsonl-pointer",
        interface: "probe-jsonl",
        bytes: row_bytes,
        issues: json!([{"message":"must be of type number","path":"/score","line":7}]),
    });
    for number in ["Infinity", "-Infinity", "1e999", "-1e999", "NaN"] {
        let bytes = format!("{{\"queries\":[{{\"docs\":[{{\"score\":{number}}}]}}]}}").into_bytes();
        // Unique names need not contain or disclose uploaded values.
        let name = match number {
            "Infinity" => "infinity",
            "-Infinity" => "minus-infinity",
            "1e999" => "overflow",
            "-1e999" => "minus-overflow",
            _ => "nan",
        };
        cases.push(fail(
            name,
            "probe-run",
            &bytes,
            "has a number that is not finite (NaN, Infinity or out of range)",
        ));
        let name = match number {
            "Infinity" => "infinity-line",
            "-Infinity" => "minus-infinity-line",
            "1e999" => "overflow-line",
            "-1e999" => "minus-overflow-line",
            _ => "nan-line",
        };
        cases.push(fail(
            name,
            "probe-jsonl",
            &[bytes, b"\n".to_vec()].concat(),
            "has a number that is not finite (NaN, Infinity or out of range) (line 1)",
        ));
    }
    let nested = [vec![b'['; 100_000], vec![b']'; 100_000]].concat();
    cases.push(fail(
        "nested",
        "probe-run",
        &nested,
        "is nested too deeply to check",
    ));
    cases.push(fail(
        "nested-line",
        "probe-jsonl",
        &[valid.as_slice(), b"\n", nested.as_slice(), b"\n"].concat(),
        "is nested too deeply to check (line 2)",
    ));
    for (name, bytes) in [
        (
            "utf16-le",
            valid.iter().flat_map(|v| [*v, 0]).collect::<Vec<_>>(),
        ),
        ("utf16-be", valid.iter().flat_map(|v| [0, *v]).collect()),
        (
            "utf16-bom",
            [
                b"\xff\xfe".as_slice(),
                valid
                    .iter()
                    .flat_map(|v| [*v, 0])
                    .collect::<Vec<_>>()
                    .as_slice(),
            ]
            .concat(),
        ),
        (
            "utf32",
            [
                b"\xff\xfe\x00\x00".as_slice(),
                valid
                    .iter()
                    .flat_map(|v| [*v, 0, 0, 0])
                    .collect::<Vec<_>>()
                    .as_slice(),
            ]
            .concat(),
        ),
    ] {
        cases.push(fail(
            name,
            "probe-row",
            &bytes,
            "is encoded as UTF-16 or UTF-32; JSON must be UTF-8",
        ));
        let line_name = match name {
            "utf16-le" => "utf16-le-line",
            "utf16-be" => "utf16-be-line",
            "utf16-bom" => "utf16-bom-line",
            _ => "utf32-line",
        };
        cases.push(fail(
            line_name,
            "probe-jsonl",
            &bytes,
            "is encoded as UTF-16 or UTF-32; JSON must be UTF-8",
        ));
    }
    cases.push(Case{name:"five-error-cap",interface:"probe-run",bytes:serde_json::to_vec(&json!({"queries":[{"docs":(0..20).map(|i|json!({"score":i.to_string()})).collect::<Vec<_>>()}]}))?,issues:json!((0..5).map(|i|json!({"message":"must be of type number","path":format!("/queries/0/docs/{i}/score")})).collect::<Vec<_>>())});
    let private_bytes = serde_json::to_vec(
        &json!({"id":"SECRET-7f3a","label":"SECRET-7f3a","kind":"SECRET-7f3a","rank":0,"SECRET-7f3a":1}),
    )?;
    let private_grant = grant(
        &mut world,
        &actors,
        &job,
        "privacy",
        "probe-private",
        &private_bytes,
    )
    .await?;
    let refused = put(&mut world, &private_grant, &private_bytes, 422).await?;
    assert!(!refused.to_string().contains("SECRET-7f3a"));
    assert_eq!(
        refused["error"]["details"]
            .as_array()
            .ok_or("issues absent")?
            .len(),
        5
    );
    assert_issues(
        &refused["error"]["details"],
        &json!([
            {"message":"reference","path":""},{"message":"reference","path":"/id"},
            {"message":"reference","path":"/kind"},{"message":"reference","path":"/label"},
            {"message":"reference","path":"/rank"}
        ]),
    )?;
    cases.push(Case {
        name: "missing-required",
        interface: "probe-private",
        bytes: b"{\"id\":\"abc\"}".to_vec(),
        issues: json!([{"message":"Python reference: required properties","path":""}]),
    });
    for case in cases {
        let grant = grant(
            &mut world,
            &actors,
            &job,
            case.name,
            case.interface,
            &case.bytes,
        )
        .await?;
        let valid = case.issues == json!([]);
        let response = put(
            &mut world,
            &grant,
            &case.bytes,
            if valid { 201 } else { 422 },
        )
        .await?;
        if valid {
            assert_eq!(response["interface"], format!("{}/v1", case.interface));
        } else {
            assert_eq!(
                response["error"]["code"], "invalid_content",
                "{}",
                case.name
            );
            assert_issues(&response["error"]["details"], &case.issues)?;
        }
    }
    world
        .finish_coverage(&actors.admin, "interface-matrix")
        .await
}

#[tokio::test]
#[ignore = "requires ordinary profile with default validation cap and normal leases"]
async fn interface_chunked_jsonl_large_stream() -> Result<()> {
    let (mut world, actors, job) = prepared(
        "interface-stream",
        vec![registration(
            "stream-lines",
            json!({"schema":{"type":"object"},"encoding":"jsonl"}),
        )],
    )
    .await?;
    let mut bytes = Vec::with_capacity(7_000_000);
    for i in 0..200_000 {
        writeln!(bytes, "{{\"query\":\"q{i}\",\"score\":{i}}}")?;
    }
    bytes.extend_from_slice(b"[\"not\",\"an\",\"object\"]\n");
    assert!(bytes.len() > 5_000_000);
    let grant = grant(
        &mut world,
        &actors,
        &job,
        "large-stream",
        "stream-lines",
        &bytes,
    )
    .await?;
    let (status, body) = support::chunked(&grant, bytes, 65_533).await?;
    assert_eq!(status, 422);
    assert_eq!(body["error"]["code"], "invalid_content");
    assert_issues(
        &body["error"]["details"],
        &json!([{"message":"Python reference: object required","path":"","line":200_001}]),
    )?;
    let bytes = "\u{feff}{\"query\":\"café 日本語 🦀\",\"score\":1}\n"
        .as_bytes()
        .to_vec();
    let grant = support::grant(
        &mut world,
        &actors,
        &job,
        "one-byte-utf8",
        "stream-lines",
        &bytes,
    )
    .await?;
    let (status, body) = support::chunked(&grant, bytes, 1).await?;
    assert_eq!(status, 201);
    assert_eq!(body["content_validated"], true);
    world
        .finish_coverage(&actors.admin, "interface-stream")
        .await
}

#[tokio::test]
#[ignore = "requires explicit API validation cap1048576 profile"]
async fn interface_above_api_parse_cap_is_not_content_validated() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_VALIDATE_JSON_MAX_BYTES")? != "1048576" {
        return Err("API parse-cap profile is required".into());
    }
    let (mut world, actors, job) = prepared(
        "interface-api-cap",
        vec![registration(
            "large-json",
            json!({"schema":{"type":"object"},"max_bytes":3_000_000}),
        )],
    )
    .await?;
    let mut bytes = b"{\"unparsed\":".to_vec();
    bytes.resize(1_100_000, b'x'); // invalid JSON is accepted above the API's explicit validation cap.
    let grant = grant(&mut world, &actors, &job, "above-cap", "large-json", &bytes).await?;
    let artifact = put(&mut world, &grant, &bytes, 201).await?;
    assert_eq!(artifact["content_validated"], false);
    assert_eq!(artifact["size_bytes"], bytes.len());
    world
        .finish_coverage(&actors.admin, "interface-api-cap")
        .await
}

#[tokio::test]
#[ignore = "requires actual CLI runner and normal lease profile"]
async fn interface_runner_files_are_checked_before_upload() -> Result<()> {
    for (mode, encoding) in [
        ("json-cap", "json"),
        ("line-cap", "jsonl"),
        ("lines", "jsonl"),
    ] {
        let (mut world, actors) = support::submitted(
            "interface-runner",
            vec![registration(
                "runner-file",
                json!({"schema":{"type":"object"},"encoding":encoding,"max_bytes":80_000_000}),
            )],
            true,
        )
        .await?;
        support::runner_file(&world, &actors, mode).await?;
        let jobs = world
            .api(lifecycle::Call::get(
                &format!("{}/jobs", lifecycle::ATTEMPT),
                format!("{}/hypotheses/1/attempts/1/jobs", world.base()),
                &actors.admin,
            ))
            .await?
            .body;
        let failed = jobs["items"]
            .as_array()
            .ok_or("jobs missing")?
            .iter()
            .find(|job| job["run_number"] == 1)
            .ok_or("initial job missing")?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error_code"], "invalid_step_output");
        assert_ne!(
            failed["error_reason"]
                .as_str()
                .ok_or("failure reason missing")?
                .trim(),
            ""
        );
        // The API does not revalidate this oversized producer output; the local
        // check_file verdict remains authoritative and causes the worker failure.
        if mode == "lines" {
            let audit = world.h.fetch_audit(&actors.admin, 0).await?.body;
            let refusal = audit["items"]
                .as_array()
                .ok_or("audit items missing")?
                .iter()
                .find(|item| {
                    item["action"] == "artifact.refused"
                        && item["new_state"]["job_id"] == failed["id"]
                })
                .ok_or("runner file refusal missing")?;
            assert_issues(
                &refusal["new_state"]["issues"],
                &json!([{"message":"Python reference: object required","path":"","line":10_001}]),
            )?;
        } else {
            let output = failed["outputs"]
                .as_array()
                .ok_or("job outputs missing")?
                .iter()
                .find(|output| output["interface"] == "ranked-run/v1")
                .ok_or("offered producer file missing")?;
            assert_eq!(output["content_validated"], false);
            assert!(output["size_bytes"].as_u64().ok_or("output size missing")? > 64 * 1024 * 1024);
        }
        world
            .finish_coverage(&actors.admin, &format!("interface-runner-{mode}"))
            .await?;
    }
    Ok(())
}
