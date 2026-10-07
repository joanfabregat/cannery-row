#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "The integration binaries share bootstrap and lease helpers"
)]
mod support;
use conformance::{CheckedResponse, Result};
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use support::{ATTEMPT, Actors, Call, JOB, Lease, World, add, object, sha, string};

const PART_BYTES: usize = 5 * 1024 * 1024;
struct BucketClient {
    endpoint: Url,
    client: Client,
}
impl BucketClient {
    fn new() -> Result<Self> {
        let endpoint = Url::parse(&std::env::var("CANNERY_CONFORMANCE_S3_ENDPOINT")?)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err("invalid conformance S3 endpoint".into());
        }
        Ok(Self {
            endpoint,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    fn url(&self, value: &Value) -> Result<Url> {
        let url = Url::parse(value.as_str().ok_or("bucket URL absent")?)
            .map_err(|_| "invalid bucket URL")?;
        if url.origin() != self.endpoint.origin()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("presigned request escaped configured bucket origin".into());
        }
        Ok(url)
    }
    async fn put(&self, request: &Value, bytes: &[u8], status: u16) -> Result<()> {
        assert_eq!(request["method"], "PUT");
        let mut outgoing = self
            .client
            .put(self.url(&request["url"])?)
            .body(bytes.to_vec());
        for (name, value) in request["headers"]
            .as_object()
            .ok_or("bucket headers absent")?
        {
            if name.eq_ignore_ascii_case("authorization")
                || name.eq_ignore_ascii_case("x-upload-token")
                || name.eq_ignore_ascii_case("cookie")
            {
                return Err("credential header in bucket plan".into());
            }
            outgoing = outgoing.header(name, string(value)?);
        }
        let response = outgoing
            .send()
            .await
            .map_err(|_| "bucket PUT transport failed")?;
        assert_eq!(response.status().as_u16(), status, "bucket PUT status");
        Ok(())
    }
    async fn download(&self, response: &CheckedResponse, bytes: &[u8]) -> Result<()> {
        let location = response
            .headers
            .get("location")
            .ok_or("redirect location absent")?
            .to_str()?;
        let downloaded = self
            .client
            .get(self.url(&json!(location))?)
            .send()
            .await
            .map_err(|_| "bucket GET transport failed")?;
        assert_eq!(downloaded.status().as_u16(), 200);
        assert_eq!(
            downloaded
                .bytes()
                .await
                .map_err(|_| "bucket GET body failed")?
                .as_ref(),
            bytes
        );
        Ok(())
    }
}

async fn capability(
    world: &mut World,
    grant: &Value,
    job: bool,
    action: &str,
    body: Option<Value>,
    status: u16,
    supplied: Option<&str>,
) -> Result<Value> {
    let url = Url::parse(&string(&grant["direct"][format!("{action}_url")])?)?;
    let mut request = world.h.request(Method::POST, url.path())?;
    if let Some(token) = supplied {
        request = request.header("X-Upload-Token", token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let template = format!(
        "/api/{}uploads/{{upload_id}}/{action}",
        if job { "job-" } else { "" }
    );
    Ok(world
        .h
        .check_response(Method::POST, &template, response, status)
        .await?
        .body)
}
async fn authorized_capability(
    world: &mut World,
    grant: &Value,
    job: bool,
    action: &str,
    body: Option<Value>,
    status: u16,
) -> Result<Value> {
    let token = string(&grant["headers"]["X-Upload-Token"])?;
    capability(world, grant, job, action, body, status, Some(&token)).await
}
async fn attempt_grant(
    world: &mut World,
    lease: &Lease,
    token: &str,
    bytes: &[u8],
    name: &str,
    kind: (&str, &str),
    mcp: bool,
) -> Result<Value> {
    let body = json!({"role":kind.0,"name":name,"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":kind.1});
    if mcp {
        let mut args = lease.attempt_args(&world.project);
        args.as_object_mut()
            .ok_or("attempt args missing")?
            .extend(body.as_object().ok_or("grant body missing")?.clone());
        world.h.call_tool(token, "create_upload", args).await
    } else {
        Ok(world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/uploads"),
                    format!("{}/uploads", world.attempt_path(lease)),
                    token,
                    body,
                    201,
                )
                .lease(lease)?,
            )
            .await?
            .body)
    }
}
async fn job_grant(
    world: &mut World,
    actors: &Actors,
    lease: &Lease,
    bytes: &[u8],
    path: &str,
    interface: Option<&str>,
    mcp: bool,
) -> Result<Value> {
    let mut body = json!({"role":"step_log","path":path,"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"text/plain"});
    if let Some(interface) = interface {
        body["role"] = json!("run");
        body["media_type"] = json!("application/json");
        body["interface"] = json!(interface);
    }
    if mcp {
        let mut args = lease.job_args(&world.project);
        args.as_object_mut()
            .ok_or("job args missing")?
            .extend(body.as_object().ok_or("job grant body missing")?.clone());
        world
            .h
            .call_tool(&actors.tester, "create_job_upload", args)
            .await
    } else {
        Ok(world
            .api(
                Call::post(
                    &format!("{JOB}/uploads"),
                    format!("{}/uploads", world.job_path(lease)?),
                    &actors.tester,
                    body,
                    201,
                )
                .lease(lease)?,
            )
            .await?
            .body)
    }
}
async fn submit_candidate(
    world: &mut World,
    actors: &Actors,
    lease: &Lease,
    artifact: &Value,
) -> Result<()> {
    let manifest = json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(artifact)]});
    let reference = world
        .h
        .call_tool(
            &actors.agent,
            "post_manifest",
            add(lease.attempt_args(&world.project), "document", manifest),
        )
        .await?;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = reference;
    sheet["provenance"]["science_revision"] = json!("1");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    let response = world
        .h
        .call_tool(
            &actors.agent,
            "submit_attempt",
            add(
                add(lease.attempt_args(&world.project), "document", sheet),
                "idempotency_key",
                json!("direct-candidate"),
            ),
        )
        .await?;
    assert_eq!(response["state"], "testing");
    Ok(())
}
async fn submitted(
    world: &mut World,
    actors: &Actors,
    bucket: &BucketClient,
) -> Result<(Lease, Value)> {
    let number = world
        .queue(actors, "Direct candidate for S3 tester")
        .await?;
    let lease = world.claim(actors, number, true).await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let granted = attempt_grant(
        world,
        &lease,
        &actors.agent,
        bytes,
        "candidate.json",
        ("candidate", "application/json"),
        true,
    )
    .await?;
    assert_eq!(granted["direct"]["transfer"], "single");
    bucket
        .put(&granted["direct"]["request"], bytes, 200)
        .await?;
    let artifact = authorized_capability(world, &granted, false, "finish", None, 201).await?;
    submit_candidate(world, actors, &lease, &artifact).await?;
    Ok((lease, artifact))
}

#[tokio::test]
#[ignore = "requires the explicit Garage conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "Direct agent and job grants are verified through their ordered public lifecycle"
)]
async fn single_put_presign_finish_and_redirected_inputs() -> Result<()> {
    let bucket = BucketClient::new()?;
    let (mut world, actors) = World::new("s3-single").await?;
    let number = world
        .queue(&actors, "Direct single PUT and capabilities")
        .await?;
    let lease = world.claim(&actors, number, true).await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let grant = attempt_grant(
        &mut world,
        &lease,
        &actors.agent,
        bytes,
        "candidate.json",
        ("candidate", "application/json"),
        true,
    )
    .await?;
    assert_eq!(grant["direct"]["transfer"], "single");
    assert_eq!(
        grant["direct"]["request"]["headers"]["Content-Length"],
        bytes.len().to_string()
    );
    assert!(grant["direct"]["request"]["headers"]["x-amz-checksum-sha256"].is_string());
    for action in ["presign", "finish"] {
        for supplied in [None, Some("cr_upl_invalid")] {
            let hidden = capability(&mut world, &grant, false, action, None, 404, supplied).await?;
            assert_eq!(hidden["error"]["code"], "not_found");
        }
    }
    let missing = authorized_capability(&mut world, &grant, false, "finish", None, 409).await?;
    assert_eq!(missing["error"]["code"], "conflict");
    let invalid = authorized_capability(
        &mut world,
        &grant,
        false,
        "presign",
        Some(json!({"part_numbers":[1]})),
        422,
    )
    .await?;
    assert_eq!(invalid["error"]["code"], "validation_failed");
    let fresh = authorized_capability(&mut world, &grant, false, "presign", None, 200).await?;
    bucket
        .put(&fresh["request"], &vec![b'x'; bytes.len()], 400)
        .await?;
    authorized_capability(&mut world, &grant, false, "finish", None, 409).await?;
    bucket.put(&fresh["request"], bytes, 200).await?;
    let artifact = authorized_capability(&mut world, &grant, false, "finish", None, 201).await?;
    assert_eq!(artifact["sha256"], sha(bytes));
    assert_eq!(artifact["size_bytes"], bytes.len());
    for action in ["presign", "finish"] {
        authorized_capability(&mut world, &grant, false, action, None, 409).await?;
    }
    let reference = world
        .h
        .call_tool(
            &actors.admin,
            "get_artifact",
            json!({"project":world.project,"artifact_id":artifact["id"]}),
        )
        .await?;
    assert_eq!(reference["artifact"]["sha256"], sha(bytes));
    let mut download = Call::get(
        "/api/projects/{slug}/artifacts/{artifact_id}",
        format!("{}/artifacts/{}", world.base(), string(&artifact["id"])?),
        &actors.admin,
    );
    download.status = 302;
    let redirected = world.api(download).await?;
    assert_eq!(redirected.headers["cache-control"], "no-store");
    bucket.download(&redirected, bytes).await?;
    let mut cached = Call::get(
        "/api/projects/{slug}/artifacts/{artifact_id}",
        format!("{}/artifacts/{}", world.base(), string(&artifact["id"])?),
        &actors.admin,
    );
    cached.status = 304;
    cached
        .headers
        .insert("If-None-Match", redirected.headers["etag"].clone());
    world.api(cached).await?;
    submit_candidate(&mut world, &actors, &lease, &artifact).await?;
    let job = world.claim_job(&actors, false, true).await?;
    let response = world
        .h
        .request(
            Method::GET,
            &format!("{}/inputs/object", world.job_path(&job)?),
        )?
        .query(&[("key", string(&artifact["storage"]["key"])?)])
        .bearer_auth(&actors.tester)
        .header("X-Lease-Token", &job.token)
        .header("X-Lease-Generation", job.generation.to_string())
        .send()
        .await?;
    let redirected = world
        .h
        .check_response(Method::GET, &format!("{JOB}/inputs/object"), response, 302)
        .await?;
    assert_eq!(redirected.headers["cache-control"], "no-store");
    bucket.download(&redirected, bytes).await?;
    let log = b"direct tester output\n";
    let output = job_grant(
        &mut world,
        &actors,
        &job,
        log,
        "fixture-scorer/step_log/s3.log",
        None,
        true,
    )
    .await?;
    assert_eq!(output["direct"]["transfer"], "single");
    for action in ["presign", "finish"] {
        for supplied in [None, Some("cr_upl_invalid")] {
            capability(&mut world, &output, true, action, None, 404, supplied).await?;
        }
    }
    authorized_capability(&mut world, &output, true, "finish", None, 409).await?;
    authorized_capability(
        &mut world,
        &output,
        true,
        "presign",
        Some(json!({"part_numbers":[1]})),
        422,
    )
    .await?;
    let presigned = authorized_capability(&mut world, &output, true, "presign", None, 200).await?;
    bucket.put(&presigned["request"], log, 200).await?;
    let verified = authorized_capability(&mut world, &output, true, "finish", None, 201).await?;
    assert_eq!(verified["sha256"], sha(log));
    for action in ["presign", "finish"] {
        authorized_capability(&mut world, &output, true, action, None, 409).await?;
    }
    let invalid = b"{\"queries\":12}";
    let refused = job_grant(
        &mut world,
        &actors,
        &job,
        invalid,
        "overlap-producer/run/run.json",
        Some("ranked-run/v1"),
        false,
    )
    .await?;
    bucket
        .put(&refused["direct"]["request"], invalid, 200)
        .await?;
    let finished = authorized_capability(&mut world, &refused, true, "finish", None, 422).await?;
    assert_eq!(finished["error"]["code"], "invalid_content");
    let detail = world
        .api(Call::get(JOB, world.job_path(&job)?, &actors.admin))
        .await?
        .body;
    assert_eq!(detail["state"], "claimed");
    assert!(
        detail["outputs"]
            .as_array()
            .ok_or("job outputs absent")?
            .iter()
            .all(|item| item["storage"]["key"] != refused["storage"]["key"])
    );
    world.finish_coverage(&actors.admin, "s3-single").await
}

#[allow(
    clippy::too_many_lines,
    reason = "Multipart validation follows upload, missing parts, refresh, and finish in order"
)]
async fn multipart(job_upload: bool, mismatch: bool) -> Result<()> {
    let bucket = BucketClient::new()?;
    let label = format!(
        "s3-multipart-{}-{}",
        if job_upload { "job" } else { "attempt" },
        if mismatch { "refusal" } else { "success" }
    );
    let (mut world, actors) = World::new(&label).await?;
    let lease = if job_upload {
        submitted(&mut world, &actors, &bucket).await?;
        world.claim_job(&actors, false, false).await?
    } else {
        let number = world
            .queue(&actors, "Multipart verified through API finish")
            .await?;
        world.claim(&actors, number, false).await?
    };
    let mut bytes = vec![b'a'; PART_BYTES];
    bytes.extend_from_slice(b"tail of the file");
    let grant = if job_upload {
        job_grant(
            &mut world,
            &actors,
            &lease,
            &bytes,
            "fixture-scorer/step_log/multipart.log",
            None,
            false,
        )
        .await?
    } else {
        attempt_grant(
            &mut world,
            &lease,
            &actors.agent,
            &bytes,
            "multipart.bin",
            ("candidate_checkpoint", "application/octet-stream"),
            false,
        )
        .await?
    };
    assert_eq!(grant["direct"]["transfer"], "multipart");
    assert_eq!(grant["direct"]["part_size"], PART_BYTES);
    assert_eq!(grant["direct"]["part_count"], 2);
    let url = Url::parse(&string(&grant["upload_url"])?)?;
    let response = world
        .h
        .request(Method::PUT, url.path())?
        .header(
            "X-Upload-Token",
            string(&grant["headers"]["X-Upload-Token"])?,
        )
        .body(bytes.clone())
        .send()
        .await?;
    world
        .h
        .check_response(
            Method::PUT,
            if job_upload {
                "/api/job-uploads/{upload_id}"
            } else {
                "/api/uploads/{upload_id}"
            },
            response,
            409,
        )
        .await?;
    for numbers in [json!([3]), json!([0]), json!([])] {
        authorized_capability(
            &mut world,
            &grant,
            job_upload,
            "presign",
            Some(json!({"part_numbers":numbers})),
            422,
        )
        .await?;
    }
    let parts = grant["direct"]["parts"]
        .as_array()
        .ok_or("multipart parts absent")?;
    assert_eq!(parts.len(), 2);
    bucket.put(&parts[0], &bytes[..PART_BYTES], 200).await?;
    let early = authorized_capability(&mut world, &grant, job_upload, "finish", None, 409).await?;
    assert_eq!(early["error"]["details"], json!({"missing_parts":[2]}));
    let refreshed = authorized_capability(
        &mut world,
        &grant,
        job_upload,
        "presign",
        Some(json!({"part_numbers":[2,2]})),
        200,
    )
    .await?;
    let parts = refreshed["parts"]
        .as_array()
        .ok_or("refreshed parts absent")?;
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["part_number"], 2);
    let tail = if mismatch {
        b"wrong final tail".as_slice()
    } else {
        &bytes[PART_BYTES..]
    };
    assert_eq!(tail.len(), bytes.len() - PART_BYTES);
    bucket.put(&parts[0], tail, 200).await?;
    let finished = authorized_capability(
        &mut world,
        &grant,
        job_upload,
        "finish",
        None,
        if mismatch { 422 } else { 201 },
    )
    .await?;
    if mismatch {
        assert_eq!(finished["error"]["code"], "validation_failed");
        let mut sent = bytes[..PART_BYTES].to_vec();
        sent.extend_from_slice(tail);
        assert_eq!(finished["error"]["details"]["received_sha256"], sha(&sent));
        let detail = world
            .api(Call::get(
                if job_upload { JOB } else { ATTEMPT },
                if job_upload {
                    world.job_path(&lease)?
                } else {
                    world.attempt_path(&lease)
                },
                &actors.admin,
            ))
            .await?
            .body;
        assert_eq!(detail["state"], "failed");
        if !job_upload {
            assert_eq!(detail["artifacts"], json!([]));
            assert_eq!(detail["failures"][0]["code"], "upload_verification_failed");
        }
    } else {
        assert_eq!(finished["sha256"], sha(&bytes));
        assert_eq!(finished["size_bytes"], bytes.len());
        authorized_capability(&mut world, &grant, job_upload, "finish", None, 409).await?;
        let mut download = Call::get(
            "/api/projects/{slug}/artifacts/{artifact_id}",
            format!("{}/artifacts/{}", world.base(), string(&finished["id"])?),
            &actors.admin,
        );
        download.status = 302;
        let redirected = world.api(download).await?;
        bucket.download(&redirected, &bytes).await?;
    }
    world.finish_coverage(&actors.admin, &label).await
}

#[tokio::test]
#[ignore = "requires Garage and five-MiB multipart threshold and part size"]
async fn multipart_attempt_uploads_success_and_digest_refusal() -> Result<()> {
    multipart(false, false).await?;
    multipart(false, true).await
}
#[tokio::test]
#[ignore = "requires Garage and five-MiB multipart threshold and part size"]
async fn multipart_job_uploads_success_and_digest_refusal() -> Result<()> {
    multipart(true, false).await?;
    multipart(true, true).await
}

#[tokio::test]
#[ignore = "requires the explicit Garage conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "A registered workflow failure creates a real verified predecessor for the retry"
)]
async fn workflow_retry_redirects_only_its_verified_predecessor_input() -> Result<()> {
    let bucket = BucketClient::new()?;
    let (mut world, actors) = World::new("s3-predecessor").await?;
    let runner = world.experimenter(&actors.admin).await?;
    let experiment: Value = serde_json::from_str(include_str!(
        "../../../examples/fixture/experiments/fixture-experiment.json"
    ))?;
    world
        .api(Call::post(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", world.base()),
            &actors.admin,
            experiment,
            201,
        ))
        .await?;
    let mut switch = Call::post(
        "/api/projects/{slug}/tracks/{track_slug}",
        format!("{}/tracks/lexical", world.base()),
        &actors.admin,
        json!({"expected_revision":1,"mode":"workflow","workflow":{"steps":[{"name":"fixture-experiment","revision":1}]},"reason":"Direct predecessor input profile"}),
        200,
    );
    switch.method = Method::PATCH;
    world.api(switch).await?;
    let number = world
        .queue(&actors, "Retain verified direct-upload predecessor")
        .await?;
    let claim = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("{}/claims", world.base()),
            &runner,
            json!({"hypothesis":number}),
            201,
        ))
        .await?
        .body;
    let lease = Lease::attempt(&claim)?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let grant = attempt_grant(
        &mut world,
        &lease,
        &runner,
        bytes,
        "candidate.json",
        ("candidate", "application/json"),
        false,
    )
    .await?;
    assert_eq!(grant["direct"]["transfer"], "single");
    bucket.put(&grant["direct"]["request"], bytes, 200).await?;
    let artifact = authorized_capability(&mut world, &grant, false, "finish", None, 201).await?;
    world.api(Call::post(&format!("{ATTEMPT}/release"),format!("{}/release",world.attempt_path(&lease)),&runner,json!({"reason":"Synthetic experiment crash retains verified candidate","code":"step_failed","step":"fixture-experiment","logs":[]}),200).lease(&lease)?).await?;
    let retry = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("{}/claims", world.base()),
            &runner,
            json!({"hypothesis":number}),
            201,
        ))
        .await?
        .body;
    let next = Lease::attempt(&retry)?;
    assert_eq!(next.document["sequence"], 2);
    let inputs = retry["workflow"]["inputs"]["predecessor"]["artifacts"]
        .as_array()
        .ok_or("predecessor input absent")?;
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["sha256"], sha(bytes));
    let template = format!("{ATTEMPT}/inputs/predecessor/{{artifact_id}}");
    let mut input = Call::get(
        &template,
        format!(
            "{}/inputs/predecessor/{}",
            world.attempt_path(&next),
            string(&artifact["id"])?
        ),
        &runner,
    )
    .lease(&next)?;
    input.status = 302;
    let redirected = world.api(input).await?;
    assert_eq!(redirected.headers["cache-control"], "no-store");
    bucket.download(&redirected, bytes).await?;
    world.finish_coverage(&actors.admin, "s3-predecessor").await
}
