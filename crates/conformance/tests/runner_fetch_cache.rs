//! A loopback GitHub fixture exercises pinned code and setup caches offline.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/runner_fetch_cache.rs");
use axum::{
    Json, Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const COMMIT: &str = "0000000000000000000000000000000000000000";
const REPOSITORY: &str = "fixture-owner/fixture-repo";
#[derive(Clone)]
struct GitHub {
    archive: Arc<Vec<u8>>,
    fetches: Arc<AtomicUsize>,
    api_calls: Arc<AtomicUsize>,
    base: String,
}
struct Server(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn github(State(state): State<GitHub>, request: Request) -> Response {
    let path = request.uri().path();
    if path == "/archive" {
        assert!(
            request.headers().get(header::AUTHORIZATION).is_none(),
            "archive redirects must not carry API credentials"
        );
        return (
            [(header::CONTENT_TYPE, "application/gzip")],
            state.archive.as_ref().clone(),
        )
            .into_response();
    }
    assert!(
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            == Some("Bearer conformance-fake-github-token"),
        "fixture GitHub API credential missing"
    );
    state.api_calls.fetch_add(1, Ordering::SeqCst);
    if path == format!("/repos/{REPOSITORY}") {
        return Json(json!({"full_name":REPOSITORY,"default_branch":"main"})).into_response();
    }
    if path == format!("/repos/{REPOSITORY}/compare/main...{COMMIT}") {
        return Json(json!({"status":"identical","commits":[]})).into_response();
    }
    if path == format!("/repos/{REPOSITORY}/tarball/{COMMIT}") {
        state.fetches.fetch_add(1, Ordering::SeqCst);
        return (
            reqwest::StatusCode::FOUND,
            [(header::LOCATION, format!("{}/archive", state.base))],
        )
            .into_response();
    }
    (
        reqwest::StatusCode::NOT_FOUND,
        Json(json!({"message":"Not Found"})),
    )
        .into_response()
}
async fn archive(root: &std::path::Path, fixture_root: &std::path::Path) -> Result<Vec<u8>> {
    let tree = root.join("archive-source/owner-repo/examples/fixture/steps");
    fs::create_dir_all(&tree)?;
    for entry in fs::read_dir(fixture_root.join("steps"))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), tree.join(entry.file_name()))?;
        }
    }
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("tar")
            .args([
                "--format=pax",
                &format!("--pax-option=comment={COMMIT}"),
                "-czf",
            ])
            .arg(root.join("fixture.tar.gz"))
            .arg("-C")
            .arg(root.join("archive-source"))
            .arg("owner-repo")
            .output()?;
        assert!(
            output.status.success(),
            "cannot package trusted fixture archive"
        );
        Ok(fs::read(root.join("fixture.tar.gz"))?)
    })
    .await?
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn runner_fetches_pinned_code_once_and_reuses_setup_cache() -> Result<()> {
    let mut world = World::new()?;
    let project = unique()?.replace("conformance", "cache");
    let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(&project);
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let work = Work(root);
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture")
        .canonicalize()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let github_base = format!("http://{}", listener.local_addr()?);
    let fetches = Arc::new(AtomicUsize::new(0));
    let api_calls = Arc::new(AtomicUsize::new(0));
    let state = GitHub {
        archive: Arc::new(archive(&work.0, &fixture_root).await?),
        fetches: fetches.clone(),
        api_calls: api_calls.clone(),
        base: github_base.clone(),
    };
    let router = Router::new()
        .fallback(routing::get(github))
        .with_state(state);
    let _server = Server(tokio::spawn(
        async move { axum::serve(listener, router).await },
    ));
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    let (token, _) = world
        .token(&admin, "runner-cache-admin", &["read", "write"])
        .await?;
    let base = format!("/api/projects/{project}");
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&token),
            Some(json!({"slug":project,"title":"Runner cache fixture"})),
            201,
        )
        .await?;
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{}", admin.id),
            None,
            Some(&token),
            Some(json!({"role":"researcher"})),
            200,
        )
        .await?;
    let agent = account(&mut world, &admin, &token, &base, "agent", "cache-agent").await?;
    let tester = account(
        &mut world,
        &admin,
        &token,
        &base,
        "tester",
        "cannery-runner",
    )
    .await?;
    private_file(&work.0.join("tester.token"), tester.as_bytes())?;
    private_file(
        &work.0.join("github.token"),
        b"conformance-fake-github-token",
    )?;
    let mut science = fixture("examples/fixture/science.json")?;
    science["code_repositories"]["candidate"] = json!([REPOSITORY]);
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            None,
            Some(&token),
            Some(science),
            201,
        )
        .await?;
    let mut producer = fixture("examples/fixture/producers/overlap-producer.json")?;
    producer["metadata"]["name"] = json!("cached-producer");
    producer["spec"]["code"] =
        json!({"repo":REPOSITORY,"commit":COMMIT,"path":"examples/fixture/steps"});
    producer["spec"]["setup"] = json!({"run":"mkdir -p \"$CR_ROOT/cache/site\" && cat requirements.txt > \"$CR_ROOT/cache/site/fixture.txt\" && ls -A \"$CR_ROOT\" > \"$CR_ROOT/cache/site/seen.txt\"","network":"none","cache":{"key_files":["requirements.txt"],"paths":["site"]},"activeDeadlineSeconds":30});
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            None,
            Some(&token),
            Some(producer),
            201,
        )
        .await?;
    world.api(Method::POST,"/api/projects/{slug}/tracks",&format!("{base}/tracks"),None,Some(&token),Some(json!({"slug":"cached","title":"Cached fixture","producer":{"name":"cached-producer","revision":1}})),201).await?;
    let mut settings = config(
        &work.0,
        &world.base,
        &project,
        &fixture_root,
        "test",
        "tester.token",
        None,
    );
    settings.push_str("\n[github]\napi_url = ");
    settings.push_str(&toml_string(&github_base));
    settings.push_str(
        "\ntoken_file = \"github.token\"\nallowed_repos = [\"fixture-owner/fixture-repo\"]\n",
    );
    fs::write(work.0.join("runner.toml"), settings)?;
    let args = vec![
        "runner".into(),
        "--config".into(),
        work.0.join("runner.toml").display().to_string(),
        "--once".into(),
    ];
    let mut setup_file = None;
    for _ in 0..2 {
        let number = submit(&mut world, &base, &agent, &token, "cached").await?;
        success(&cli(args.clone(), None).await?, "completed");
        inspect_test_job(&mut world, &base, number, &token, "cached-producer").await?;
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the pinned code tree must be fetched once"
        );
        assert_eq!(
            api_calls.load(Ordering::SeqCst),
            3,
            "cache hits must avoid verification and download calls"
        );
        let entries = fs::read_dir(work.0.join("cache/setup"))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(entries.len(), 1, "one setup cache must be published");
        let file = entries[0].path().join("site/fixture.txt");
        assert_eq!(
            fs::read(&file)?,
            fs::read(fixture_root.join("steps/requirements.txt"))?
        );
        let seen = fs::read_to_string(entries[0].path().join("site/seen.txt"))?;
        assert!(seen.contains("code"));
        assert!(seen.contains("cache"));
        assert!(!seen.contains("job.json"));
        assert!(!seen.contains("inputs"));
        let modified = fs::metadata(&file)?.modified()?;
        if let Some(previous) = setup_file {
            assert_eq!(modified, previous, "a setup cache hit must not rerun setup");
        }
        setup_file = Some(modified);
        let code = work.0.join(format!(
            "cache/code/{REPOSITORY}/{COMMIT}/examples/fixture/steps/produce_overlap.py"
        ));
        assert_eq!(fs::metadata(code)?.permissions().mode() & 0o777, 0o444);
    }
    world.export()?;
    Ok(())
}
