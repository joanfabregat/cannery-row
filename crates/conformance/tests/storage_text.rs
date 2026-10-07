#![forbid(unsafe_code)]
#[path = "support/import_support.rs"]
#[allow(
    dead_code,
    reason = "Reuse the HTTP/CLI bootstrap without its separate import assertions"
)]
mod import_support;

use conformance::Result;
use import_support::{World, string};
use reqwest::{Client, Method, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const ENTITIES: [&str; 13] = [
    "config",
    "track",
    "hypothesis",
    "hypothesis_revision",
    "attempt",
    "policy",
    "import_entry",
    "report",
    "review_case",
    "decision",
    "measurement",
    "comparison",
    "evidence",
];
const CASES: [&str; 4] = ["date", "instant", "offset", "same-day"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Session {
    timezone: String,
    date_style: String,
    read_only: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Row {
    entity: String,
    key: Vec<String>,
    values: BTreeMap<String, Option<String>>,
    captured: BTreeMap<String, Option<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    version: u64,
    project: String,
    session: Session,
    counts: BTreeMap<String, u64>,
    rows: Vec<Row>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    version: u64,
    source: String,
    fixture_sha256: String,
    #[serde(default)]
    serialization_manifest_sha256: Option<String>,
    cases: BTreeMap<String, Projection>,
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Comparable {
    entity: String,
    key: Vec<String>,
    values: BTreeMap<String, Option<String>>,
    captured_columns: Vec<String>,
}

impl Projection {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.session
                != (Session {
                    timezone: "UTC".into(),
                    date_style: "ISO, YMD".into(),
                    read_only: "on".into(),
                })
        {
            return Err("storage projection version/read-only session metadata differs".into());
        }
        let mut counts: BTreeMap<String, u64> = ENTITIES
            .iter()
            .map(|entity| ((*entity).into(), 0))
            .collect();
        if self.rows.len() > 2000 || self.counts.keys().ne(counts.keys()) {
            return Err("storage projection entity allowlist/row bound differs".into());
        }
        for row in &self.rows {
            *counts
                .get_mut(&row.entity)
                .ok_or("unrecognized storage entity")? += 1;
            if row.key.is_empty() {
                return Err("storage correlation key absent".into());
            }
        }
        if self.counts != counts {
            return Err("storage counts differ from row multiplicity".into());
        }
        Ok(())
    }
    fn comparable(&self) -> Vec<Comparable> {
        let mut rows: Vec<_> = self
            .rows
            .iter()
            .map(|row| Comparable {
                entity: row.entity.clone(),
                key: row.key.clone(),
                values: row.values.clone(),
                captured_columns: row.captured.keys().cloned().collect(),
            })
            .collect();
        rows.sort();
        rows
    }
}

fn compare(reference: &Projection, actual: &Projection) -> Result<()> {
    reference.validate()?;
    actual.validate()?;
    if reference.project != actual.project
        || reference.session != actual.session
        || reference.counts != actual.counts
    {
        return Err("storage projection identity/session/counts differ".into());
    }
    let expected = reference.comparable();
    let received = actual.comparable();
    if expected.len() != received.len() {
        return Err("storage projection multiplicity differs".into());
    }
    for (expected, received) in expected.iter().zip(&received) {
        if expected != received {
            // Do not dump source content or captured IDs; identify only the entity and key.
            return Err(format!(
                "storage text differs for {} {:?}",
                expected.entity, expected.key
            )
            .into());
        }
    }
    Ok(())
}

fn compare_complete(reference: &Projection, actual: &Projection) -> Result<()> {
    compare(reference, actual)?;
    let captured = |projection: &Projection| {
        let mut rows = projection
            .rows
            .iter()
            .map(|row| (row.entity.clone(), row.key.clone(), row.captured.clone()))
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    if captured(reference) != captured(actual) {
        return Err("fixed serialization fixture stored text differs".into());
    }
    Ok(())
}

fn capture_projection(path: &Path, projection: &impl Serialize) -> Result<()> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temporary = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(projection)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        // This exact temporary file was created by this call; never follow an existing target.
        fs::remove_file(&temporary)?;
    }
    result
}

#[tokio::test]
#[ignore = "requires URL-driven API/OIDC and CLI plus explicit storage reference mode"]
#[allow(
    clippy::too_many_lines,
    reason = "keep the four-case reference protocol and artifact write in one visible sequence"
)]
async fn actual_import_storage_text_record_or_compare() -> Result<()> {
    let mode = std::env::var("CANNERY_CONFORMANCE_STORAGE_TEXT_MODE")?;
    let serialization_manifest_sha256 =
        if std::env::var("CANNERY_CONFORMANCE_SERIALIZATION_FIXTURES").as_deref() == Ok("true") {
            let digest = std::env::var("CANNERY_CONFORMANCE_SERIALIZATION_MANIFEST_SHA256")?;
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("serialization fixture manifest digest is invalid".into());
            }
            Some(digest)
        } else {
            None
        };
    if !["record", "compare"].contains(&mode.as_str()) {
        return Err("storage text mode must be record or compare".into());
    }
    let reference_path =
        PathBuf::from(std::env::var("CANNERY_CONFORMANCE_STORAGE_TEXT_REFERENCE")?);
    if !reference_path.is_absolute() {
        return Err("storage reference path must be absolute".into());
    }
    let baseline: Option<Reference> = if mode == "compare" {
        let reference: Reference = serde_json::from_slice(&fs::read(&reference_path)?)?;
        if reference.version != 1
            || reference.source != "explicit-python-baseline"
            || reference.serialization_manifest_sha256 != serialization_manifest_sha256
            || reference.cases.keys().map(String::as_str).ne(CASES
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter())
        {
            return Err("storage reference version/source/case inventory differs".into());
        }
        Some(reference)
    } else {
        None
    };
    let projections = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?)
        .join("storage-text-projections");
    match fs::DirBuilder::new().mode(0o700).create(&projections) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&projections)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err("storage projection directory must be private and direct".into());
    }
    let mut world = World::new().await?;
    let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
    login(
        &mut world,
        &base,
        "storage-ana",
        "storage-ana@conformance.test",
        &["read"],
    )
    .await?;
    login(
        &mut world,
        &base,
        "storage-ben",
        "storage-ben@conformance.test",
        &["read"],
    )
    .await?;
    let write_only = login(
        &mut world,
        &base,
        "conformance-admin",
        "admin@conformance.test",
        &["write"],
    )
    .await?;
    let mut science: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/science.json"))?;
    science["hypothesis_fields"]["properties"]["serialization_probe"] = serde_json::from_str(
        r#"{"enum":[{"longer_key":1,"a":1.0,"negative_zero":-0.0,"small":1e-100,"large":1e100,"unicode":"café 東京 🦀","control":"line\n\t","null":null,"empty":{},"list":[true,false]}]}"#,
    )?;
    fs::write(&world.science, serde_json::to_vec_pretty(&science)?)?;
    let mut inputs = ring::digest::Context::new(&ring::digest::SHA256);
    hash_part(&mut inputs, b"science.json", &fs::read(&world.science)?);
    let mut actual = BTreeMap::new();
    for case in CASES {
        let slug = format!("storage-text-{case}");
        let bundle = world.directory.join(case);
        copy_bundle(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/import"),
            &bundle,
        )?;
        import_support::edit(
            &bundle.join("project.yaml"),
            "slug: retrieval-history",
            &format!("slug: {slug}"),
        )?;
        prepare_case(&bundle, case)?;
        hash_tree(&mut inputs, &bundle, &format!("{case}/"))?;
        world.run(&bundle, &slug, Some(&world.science), &[], 0)?;
        let path = format!("/__conformance/storage/projects/{slug}");
        if case == "date" {
            for (token, status) in [
                (None, 401),
                (Some(world.ana.read_token.as_str()), 403),
                (Some(write_only.as_str()), 403),
            ] {
                projection_error(&world, &path, token, status).await?;
            }
            projection_error(&world, "/__conformance/audit", Some(&write_only), 403).await?;
            projection_error(
                &world,
                "/__conformance/storage/projects/no-storage-project",
                Some(&world.admin.read_token),
                404,
            )
            .await?;
        }
        let response = world
            .h
            .request(Method::GET, &path)?
            .bearer_auth(&world.admin.read_token)
            .send()
            .await?;
        if response.status() != 200 {
            return Err(format!("storage projection returned {}", response.status()).into());
        }
        let projection: Projection = response.json().await?;
        projection.validate()?;
        // Preserve the bounded, auth-free observation before any value/captured-field assertion.
        capture_projection(&projections.join(format!("{case}.json")), &projection)?;
        check_source_values(&projection, case)?;
        if let Some(reference) = &baseline {
            let expected = reference.cases.get(case).ok_or("reference case missing")?;
            if serialization_manifest_sha256.is_some() {
                compare_complete(expected, &projection)?;
            } else {
                compare(expected, &projection)?;
            }
        }
        actual.insert(case.to_owned(), projection);
    }
    let fixture_sha256 = hex(inputs.finish().as_ref());
    if let Some(reference) = &baseline
        && reference.fixture_sha256 != fixture_sha256
    {
        return Err("storage reference fixture bytes changed".into());
    }
    let reference = Reference {
        version: 1,
        source: if mode == "record" {
            "explicit-python-baseline"
        } else {
            "comparison-observation"
        }
        .into(),
        fixture_sha256,
        serialization_manifest_sha256,
        cases: actual,
    };
    let output = if mode == "record" {
        reference_path
    } else {
        let name = reference_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("reference filename absent")?;
        reference_path.with_file_name(format!("{name}.actual.json"))
    };
    capture_projection(&output, &reference)?;
    // Captured now()/UUID-dependent columns stay in both artifacts, but are not equality assertions.
    world.audit(0).await?;
    fs::write(
        PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?).join("storage-text.json"),
        serde_json::to_vec_pretty(world.h.coverage())?,
    )?;
    Ok(())
}

fn check_source_values(projection: &Projection, case: &str) -> Result<()> {
    let report = projection
        .rows
        .iter()
        .find(|row| row.entity == "report" && row.key == ["H-001", "1", "1"])
        .ok_or("historical report absent")?;
    let null: Option<String> = None;
    if ["date", "same-day"].contains(&case) {
        let date = if case == "date" {
            "2026-09-28"
        } else {
            "2025-01-09"
        };
        assert_eq!(report.values.get("written_on"), Some(&Some(date.into())));
        assert_eq!(report.values.get("written_at"), Some(&null));
    } else {
        let instant = if case == "instant" {
            "2025-01-10 00:00:00+00"
        } else {
            "2025-01-09 15:12:00.999999+00"
        };
        assert_eq!(report.values.get("written_on"), Some(&null));
        assert_eq!(report.values.get("written_at"), Some(&Some(instant.into())));
    }
    if case == "same-day" {
        let decision = projection
            .rows
            .iter()
            .find(|row| row.entity == "decision" && row.key.first().is_some_and(|id| id == "H-001"))
            .ok_or("historical decision absent")?;
        assert_eq!(
            decision.values.get("decided_at"),
            Some(&Some("2025-01-09 15:12:00+00".into()))
        );
    }
    if case == "offset" {
        let attempt = projection
            .rows
            .iter()
            .find(|row| row.entity == "attempt" && row.key == ["H-001", "1", "1"])
            .ok_or("historical attempt absent")?;
        assert_eq!(
            attempt.values.get("started_at"),
            Some(&Some("2025-01-09 14:00:00.123456+00".into()))
        );
        assert_eq!(
            attempt.values.get("finished_at"),
            Some(&Some("2025-01-09 14:12:00.654321+00".into()))
        );
    }
    let config = projection
        .rows
        .iter()
        .find(|row| row.entity == "config")
        .ok_or("science absent")?;
    let content = config
        .values
        .get("content")
        .and_then(Option::as_deref)
        .ok_or("science text absent")?;
    assert!(content.contains("café 東京 🦀"));
    assert!(content.contains("\\n\\t"));
    let evidence = projection
        .rows
        .iter()
        .find(|row| row.entity == "evidence")
        .ok_or("import evidence absent")?;
    assert!(
        evidence
            .captured
            .get("content")
            .and_then(Option::as_ref)
            .is_some()
    );
    assert!(!evidence.values.contains_key("content"));
    Ok(())
}

fn prepare_case(bundle: &Path, case: &str) -> Result<()> {
    let hypothesis = bundle.join("hypotheses/H-001.yaml");
    match case {
        "instant" => import_support::edit(
            &hypothesis,
            "written_at: 2026-09-28",
            "written_at: 2025-01-10T00:00:00Z",
        )?,
        "offset" => {
            import_support::edit(
                &hypothesis,
                "started_at: 2025-01-09T14:00:00Z",
                "started_at: 2025-01-09T16:00:00.123456+02:00",
            )?;
            import_support::edit(
                &hypothesis,
                "finished_at: 2025-01-09T15:12:00Z",
                "finished_at: 2025-01-09T16:12:00.654321+02:00",
            )?;
            import_support::edit(
                &hypothesis,
                "written_at: 2026-09-28",
                "written_at: 2025-01-09T19:12:00.999999+04:00",
            )?;
        }
        "same-day" => {
            import_support::edit(
                &hypothesis,
                "evaluated_at: 2025-01-10",
                "evaluated_at: 2025-01-09",
            )?;
            import_support::edit(
                &hypothesis,
                "decided_at: 2025-01-11",
                "decided_at: 2025-01-09",
            )?;
            import_support::edit(
                &hypothesis,
                "written_at: 2026-09-28",
                "written_at: 2025-01-09",
            )?;
        }
        "date" => {}
        _ => return Err("unknown source fixture case".into()),
    }
    Ok(())
}

fn copy_bundle(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let output = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_bundle(&entry.path(), &output)?;
        } else if entry.file_type()?.is_file() {
            fs::write(
                output,
                fs::read_to_string(entry.path())?
                    .replace("ana@example.org", "storage-ana@conformance.test")
                    .replace("ben@example.org", "storage-ben@conformance.test"),
            )?;
        } else {
            return Err("unexpected source fixture type".into());
        }
    }
    Ok(())
}
fn hash_part(context: &mut ring::digest::Context, name: &[u8], bytes: &[u8]) {
    context.update(&(name.len() as u64).to_be_bytes());
    context.update(name);
    context.update(&(bytes.len() as u64).to_be_bytes());
    context.update(bytes);
}
fn hash_tree(context: &mut ring::digest::Context, root: &Path, prefix: &str) -> Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = format!(
            "{prefix}{}",
            entry
                .file_name()
                .to_str()
                .ok_or("fixture filename not UTF-8")?
        );
        if entry.file_type()?.is_dir() {
            hash_tree(context, &entry.path(), &format!("{name}/"))?;
        } else {
            hash_part(context, name.as_bytes(), &fs::read(entry.path())?);
        }
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

async fn projection_error(
    world: &World,
    path: &str,
    token: Option<&str>,
    status: u16,
) -> Result<()> {
    let mut request = world.h.request(Method::GET, path)?;
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    assert_eq!(response.status().as_u16(), status);
    let body: Value = response.json().await?;
    let code = match status {
        401 => "unauthenticated",
        403 => "forbidden",
        404 => "not_found",
        _ => return Err("unexpected expected status".into()),
    };
    assert_eq!(body["error"]["code"], code);
    Ok(())
}
async fn login(
    world: &mut World,
    base: &str,
    subject: &str,
    email: &str,
    scopes: &[&str],
) -> Result<String> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client.get(format!("{base}/auth/login")).send().await?;
    if response.status() != 302 {
        return Err("storage login did not redirect".into());
    }
    let authorization = response
        .headers()
        .get("location")
        .ok_or("login location absent")?
        .to_str()?;
    let binding = cookie(response.headers(), "cr_login")?;
    let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
    let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
    let approved: Value = client.post(format!("{oidc}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":authorization,"claims":{"sub":subject,"email":email,"email_verified":true,"name":subject}})).send().await?.error_for_status()?.json().await?;
    let response = client
        .get(format!("{base}/auth/callback"))
        .header("cookie", binding)
        .query(&[
            ("state", string(&approved["state"])?),
            ("code", string(&approved["code"])?),
        ])
        .send()
        .await?;
    if response.status() != 302 {
        return Err("storage callback did not redirect".into());
    }
    let session = cookie(response.headers(), "cr_session")?;
    let response = client
        .get(format!("{base}/api/me"))
        .header("cookie", &session)
        .send()
        .await?;
    let checked = world
        .h
        .check_response(Method::GET, "/api/me", response, 200)
        .await?;
    let csrf = string(&checked.body["csrf_token"])?;
    let response = client
        .post(format!("{base}/api/tokens"))
        .header("cookie", session)
        .header("X-CSRF-Token", csrf)
        .json(&json!({"name":"storage-text","expires_in_days":1,"scopes":scopes}))
        .send()
        .await?;
    let checked = world
        .h
        .check_response(Method::POST, "/api/tokens", response, 201)
        .await?;
    string(&checked.body["token"])
}
fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    for value in headers.get_all("set-cookie") {
        let cookie = value.to_str()?.split(';').next().ok_or("invalid cookie")?;
        if cookie.starts_with(&format!("{name}=")) {
            return Ok(cookie.to_owned());
        }
    }
    Err("login cookie absent".into())
}

#[test]
fn comparator_preserves_text_and_multiplicity_but_captures_volatile_fields() -> Result<()> {
    let counts: BTreeMap<String, u64> = ENTITIES
        .iter()
        .map(|entity| ((*entity).into(), u64::from(*entity == "config")))
        .collect();
    let projection = Projection {
        version: 1,
        project: "test".into(),
        session: Session {
            timezone: "UTC".into(),
            date_style: "ISO, YMD".into(),
            read_only: "on".into(),
        },
        counts,
        rows: vec![Row {
            entity: "config".into(),
            key: vec!["science".into(), "1".into()],
            values: BTreeMap::from([
                ("content".into(), Some("{\"n\": 1.0}".into())),
                (
                    "timestamp".into(),
                    Some("2025-01-01 00:00:00.123456+00".into()),
                ),
            ]),
            captured: BTreeMap::from([("created_at".into(), Some("dynamic A".into()))]),
        }],
    };
    let mut changed = projection.clone();
    changed.rows[0]
        .captured
        .insert("created_at".into(), Some("dynamic B".into()));
    compare(&projection, &changed)?;
    assert!(compare_complete(&projection, &changed).is_err());
    compare_complete(&projection, &projection)?;
    changed.rows[0].captured.clear();
    assert!(compare(&projection, &changed).is_err());
    changed = projection.clone();
    changed.rows[0]
        .values
        .insert("content".into(), Some("{\"n\": 1}".into()));
    assert!(compare(&projection, &changed).is_err());
    changed = projection.clone();
    changed.rows[0].values.insert(
        "timestamp".into(),
        Some("2025-01-01 00:00:00.123+00".into()),
    );
    assert!(compare(&projection, &changed).is_err());
    changed = projection.clone();
    changed.rows.push(changed.rows[0].clone());
    changed.counts.insert("config".into(), 2);
    assert!(compare(&projection, &changed).is_err());
    changed = projection.clone();
    changed.session.read_only = "off".into();
    assert!(compare(&projection, &changed).is_err());
    // A failed comparison retains exact synthetic fields privately, including a safe replacement.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("storage-text-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory)?;
    let captured = directory.join("projection.json");
    let outside = directory.join("untouched.txt");
    let result = (|| -> Result<()> {
        fs::write(&outside, b"synthetic sentinel")?;
        std::os::unix::fs::symlink(&outside, &captured)?;
        capture_projection(&captured, &changed)?;
        assert_eq!(fs::metadata(&captured)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&outside)?, b"synthetic sentinel");
        let stored: Projection = serde_json::from_slice(&fs::read(&captured)?)?;
        assert!(compare(&projection, &stored).is_err());
        assert_eq!(stored.rows[0].captured, changed.rows[0].captured);
        assert_eq!(stored.rows[0].values, changed.rows[0].values);
        capture_projection(&captured, &projection)?;
        assert_eq!(fs::metadata(&captured)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::read_dir(&directory)?.count(),
            2,
            "no temporary capture left"
        );
        Ok(())
    })();
    fs::remove_dir_all(&directory)?;
    result?;
    Ok(())
}
