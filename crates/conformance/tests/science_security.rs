#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Shared HTTP-only fixture helpers")]
mod support;

use conformance::Result;
use serde_json::{Value, json};
use support::{Call, World};

fn science() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../examples/fixture/science.json"
    ))?)
}
fn producer() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../examples/fixture/producers/overlap-producer.json"
    ))?)
}
fn replace(document: &mut Value, pointer: &str, value: Value) -> Result<()> {
    *document
        .pointer_mut(pointer)
        .ok_or("fixture pointer absent")? = value;
    Ok(())
}
fn append(document: &mut Value, pointer: &str, value: Value) -> Result<()> {
    document
        .pointer_mut(pointer)
        .and_then(Value::as_array_mut)
        .ok_or("fixture array absent")?
        .push(value);
    Ok(())
}
async fn rejected(
    world: &mut World,
    token: &str,
    endpoint: &str,
    document: Value,
    path: &str,
) -> Result<()> {
    let template = if endpoint == "config/science" {
        "/api/projects/{slug}/config/{kind}"
    } else {
        "/api/projects/{slug}/producers"
    };
    let response = world
        .api(Call::post(
            template,
            format!("{}/{endpoint}", world.base()),
            token,
            document,
            422,
        ))
        .await?;
    assert_eq!(response.body["error"]["code"], "validation_failed");
    assert_eq!(response.body["error"]["details"][0]["path"], path);
    Ok(())
}

#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
#[allow(
    clippy::too_many_lines,
    reason = "Source-derived science and step mutation matrix"
)]
async fn science_cross_references_and_candidate_trust_boundaries() -> Result<()> {
    let (mut world, actors) = World::new("science-security").await?;
    for (array, path) in [
        ("metrics", "/metrics"),
        ("baselines", "/baselines"),
        ("validators", "/validators"),
    ] {
        let mut document = science()?;
        let duplicate = document[array][0].clone();
        append(&mut document, &format!("/{array}"), duplicate)?;
        rejected(&mut world, &actors.admin, "config/science", document, path).await?;
    }
    for (pointer, value, path) in [
        (
            "/interfaces/0/validator",
            json!("unknown-validator"),
            "/interfaces/0/validator",
        ),
        (
            "/validators/0/spec/inputs/artifacts/0/interface",
            json!("per-query-results/v1"),
            "/interfaces/0/validator",
        ),
        (
            "/validators/0/spec/activeDeadlineSeconds",
            json!(1_000_000),
            "/validators/0/spec/activeDeadlineSeconds",
        ),
        (
            "/scorer/spec/container/resources/limits/cpu",
            json!("2500m"),
            "/scorer/spec/container/resources/limits/cpu",
        ),
    ] {
        let mut document = science()?;
        replace(&mut document, pointer, value)?;
        rejected(&mut world, &actors.admin, "config/science", document, path).await?;
    }
    for (pointer, value, path) in [
        (
            "/spec/container/resources/limits/cpu",
            json!("2500m"),
            "/spec/container/resources/limits/cpu",
        ),
        (
            "/spec/container/env/0/value",
            json!("https://user:synthetic-password@example.invalid/x"),
            "/spec/container/env/0/value",
        ),
        (
            "/spec/container/env/0/name",
            json!("NVIDIA_VISIBLE_DEVICES"),
            "/spec/container/env/0/name",
        ),
        (
            "/spec/activeDeadlineSeconds",
            json!(601),
            "/spec/activeDeadlineSeconds",
        ),
        (
            "/spec/outputs/artifacts/0/name",
            json!("queries"),
            "/spec/outputs/artifacts/0/name",
        ),
        (
            "/spec/inputs/artifacts/1/name",
            json!("corpus"),
            "/spec/inputs/artifacts/1/name",
        ),
        (
            "/spec/outputs/artifacts/0/interface",
            json!("ranked-run/v2"),
            "/spec/outputs/artifacts/0/interface",
        ),
        ("/spec/role", json!("scorer"), "/spec/outputs/artifacts"),
    ] {
        let mut document = producer()?;
        replace(&mut document, pointer, value)?;
        rejected(&mut world, &actors.admin, "producers", document, path).await?;
    }
    for (input, path) in [
        (
            json!({"name":"qrels","from":"dataset","path":"/cr/inputs/qrels"}),
            "/spec/inputs/artifacts/2",
        ),
        (
            json!({"name":"labels","from":"dataset","id":"qrels","path":"/cr/inputs/labels"}),
            "/spec/inputs/artifacts/2",
        ),
        (
            json!({"name":"control","from":"baseline","path":"/cr/inputs/control"}),
            "/spec/inputs/artifacts/2/name",
        ),
        (
            json!({"name":"control","from":"baseline","id":"summit","path":"/cr/inputs/control"}),
            "/spec/inputs/artifacts/2/id",
        ),
        (
            json!({"name":"prior","from":"step","interface":"ranked-run/v1","path":"/cr/inputs/prior"}),
            "/spec/inputs/artifacts/2/from",
        ),
    ] {
        let mut document = producer()?;
        append(&mut document, "/spec/inputs/artifacts", input)?;
        rejected(&mut world, &actors.admin, "producers", document, path).await?;
    }
    let mut document = producer()?;
    document["spec"]["inputs"]["artifacts"][1]["id"] = json!("nanobeir-corpus");
    rejected(
        &mut world,
        &actors.admin,
        "producers",
        document,
        "/spec/inputs/artifacts/1/id",
    )
    .await?;
    let mut document = producer()?;
    document["spec"]["code"] = json!({"repo":"joanfabregat/cannery-row","commit":"a".repeat(40)});
    document["spec"]["setup"] = json!({"run":"synthetic setup command","cache":{"key_files":[]},"activeDeadlineSeconds":601});
    rejected(
        &mut world,
        &actors.admin,
        "producers",
        document,
        "/spec/setup/activeDeadlineSeconds",
    )
    .await?;
    let unchanged = world
        .api(Call::get(
            "/api/projects/{slug}/config/{kind}/latest",
            format!("{}/config/science/latest", world.base()),
            &actors.admin,
        ))
        .await?;
    assert_eq!(unchanged.body["revision"], 1);
    assert_eq!(unchanged.body["content"], science()?);
    let registered = world
        .api(Call::get(
            "/api/projects/{slug}/producers",
            format!("{}/producers", world.base()),
            &actors.admin,
        ))
        .await?;
    assert_eq!(
        registered.body["items"]
            .as_array()
            .ok_or("producer list absent")?
            .len(),
        1
    );
    world
        .finish_coverage(&actors.admin, "science-security")
        .await
}

#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
#[allow(
    clippy::too_many_lines,
    reason = "Trust classes and alias cases share pinned science"
)]
async fn repository_allowlists_hyphenated_inputs_and_resource_quantities() -> Result<()> {
    let (mut world, actors) = World::new("science-aliases").await?;
    let mut revision = science()?;
    revision["code_repositories"] = json!({
        "candidate":["Pilchards/Candidates"],"trusted":["pilchards/judges"]
    });
    append(
        &mut revision,
        "/datasets",
        json!({"id":"nanobeir-queries","revision":"queries-r1","held_out_labels":false}),
    )?;
    append(
        &mut revision,
        "/datasets",
        json!({"id":"nanobeir-qrels","revision":"qrels-r1","held_out_labels":true}),
    )?;
    let response = world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            revision.clone(),
            201,
        ))
        .await?;
    assert_eq!(response.body["revision"], 2);
    for cpu in ["500m", "2", "2000m"] {
        let mut manifest = producer()?;
        manifest["spec"]["container"]["resources"]["limits"]["cpu"] = json!(cpu);
        manifest["spec"]["code"] = json!({"repo":"pilchards/candidates","commit":"a".repeat(40)});
        manifest["spec"]["inputs"]["artifacts"][1]["id"] = json!("nanobeir-queries");
        let created = world
            .api(Call::post(
                "/api/projects/{slug}/producers",
                format!("{}/producers", world.base()),
                &actors.admin,
                manifest.clone(),
                201,
            ))
            .await?;
        assert_eq!(created.body["content"], manifest);
    }
    for repo in ["pilchards/judges", "pilchards/elsewhere"] {
        let mut manifest = producer()?;
        manifest["spec"]["code"] = json!({"repo":repo,"commit":"a".repeat(40)});
        rejected(
            &mut world,
            &actors.admin,
            "producers",
            manifest,
            "/spec/code/repo",
        )
        .await?;
    }
    for role in ["producer", "experiment"] {
        let mut manifest = producer()?;
        manifest["spec"]["role"] = json!(role);
        manifest["spec"]["inputs"]["artifacts"][1]["id"] = json!("nanobeir-qrels");
        let endpoint = if role == "producer" {
            "producers"
        } else {
            "experiment-steps"
        };
        let template = if role == "producer" {
            "/api/projects/{slug}/producers"
        } else {
            "/api/projects/{slug}/experiment-steps"
        };
        let response = world
            .api(Call::post(
                template,
                format!("{}/{endpoint}", world.base()),
                &actors.admin,
                manifest,
                422,
            ))
            .await?;
        assert_eq!(
            response.body["error"]["details"][0]["path"],
            "/spec/inputs/artifacts/1"
        );
    }
    revision["scorer"]["spec"]["code"] = json!({"repo":"pilchards/judges","commit":"a".repeat(40)});
    revision["scorer"]["spec"]["inputs"]["artifacts"][1]["id"] = json!("nanobeir-qrels");
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            revision.clone(),
            201,
        ))
        .await?;
    revision["scorer"]["spec"]["code"]["repo"] = json!("pilchards/candidates");
    rejected(
        &mut world,
        &actors.admin,
        "config/science",
        revision.clone(),
        "/scorer/spec/code/repo",
    )
    .await?;
    revision["scorer"]["spec"]
        .as_object_mut()
        .ok_or("scorer spec absent")?
        .remove("code");
    revision
        .as_object_mut()
        .ok_or("science absent")?
        .remove("code_repositories");
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            revision,
            201,
        ))
        .await?;
    let mut manifest = producer()?;
    manifest["spec"]["code"] = json!({"repo":"pilchards/candidates","commit":"a".repeat(40)});
    rejected(
        &mut world,
        &actors.admin,
        "producers",
        manifest,
        "/spec/code/repo",
    )
    .await?;
    for value in [
        "cr_svc_synthetic",
        "-----BEGIN RSA PRIVATE KEY-----",
        "AKIAABCDEFGHIJKLMNOP",
    ] {
        let mut manifest = producer()?;
        manifest["spec"]["container"]["env"][0]["value"] = json!(value);
        rejected(
            &mut world,
            &actors.admin,
            "producers",
            manifest,
            "/spec/container/env/0/value",
        )
        .await?;
    }
    world
        .finish_coverage(&actors.admin, "science-aliases")
        .await
}
