mod support;
use cannery_imports::{Bundle, Error, ImportOptions, read_bundle, run_import};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{collections::BTreeMap, time::Duration};
use support::{Directory, LIMITS, Result};
async fn snapshot(pool: &PgPool) -> Result<BTreeMap<String, Vec<String>>> {
    let mut snapshot = BTreeMap::new();
    for table in [
        "users",
        "projects",
        "memberships",
        "config_revisions",
        "import_entries",
        "historical_policies",
        "tracks",
        "hypotheses",
        "hypothesis_revisions",
        "hypothesis_relations",
        "attempts",
        "artifacts",
        "phase_outputs",
        "measurements",
        "attempt_failures",
        "review_cases",
        "decisions",
        "audit_events",
    ] {
        let query =
            format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY row_to_json(t)::text");
        let rows = sqlx::query_scalar::<_, String>(&query)
            .fetch_all(pool)
            .await?;
        snapshot.insert(table.into(), rows);
    }
    Ok(snapshot)
}
fn options(science: Option<&Value>, dry_run: bool, allow_missing: bool) -> ImportOptions<'_> {
    ImportOptions {
        slug: "retrieval-history",
        science_document: science,
        dry_run,
        allow_missing,
    }
}
async fn count(pool: &PgPool, table: &str) -> Result<i64> {
    Ok(
        sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await?,
    )
}
fn fixture(directory: &Directory, context: &cannery_imports::ImportContext) -> Result<Bundle> {
    Ok(read_bundle(&directory.0, LIMITS, &context.contracts)?)
}
#[tokio::test]
#[ignore = "requires guarded isolated native-migrated PostgreSQL"]
#[allow(clippy::too_many_lines)] // One owned database proves the complete append/replay/recovery sequence.
async fn imports_preserve_history_transactions_and_concurrency() -> Result {
    let uri = std::env::var("IMPORT_TEST_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&uri)
        .await?;
    sqlx::query("INSERT INTO users(issuer,subject,email,email_verified) VALUES('fixture','ana','ana@example.org',true),('fixture','ben','ben@example.org',true)").execute(&pool).await?;
    let context = support::context()?;
    let directory = Directory::new()?;
    support::copy(&support::root().join("examples/import"), &directory.0)?;
    let bundle = fixture(&directory, &context)?;
    let science: Value = serde_json::from_slice(&std::fs::read(
        support::root().join("examples/fixture/science.json"),
    )?)?;
    let before = snapshot(&pool).await?;
    sqlx::query("UPDATE users SET email_verified=false WHERE subject='ben'")
        .execute(&pool)
        .await?;
    assert!(matches!(
        run_import(
            &pool,
            &bundle,
            options(Some(&science), false, false),
            &context
        )
        .await,
        Err(Error::Refused(_))
    ));
    sqlx::query("UPDATE users SET email_verified=true WHERE subject='ben'")
        .execute(&pool)
        .await?;
    assert_eq!(snapshot(&pool).await?, before);
    sqlx::query("INSERT INTO users(issuer,subject,email,email_verified) VALUES('fixture','duplicate','ANA@example.org',true)").execute(&pool).await?;
    assert!(matches!(
        run_import(
            &pool,
            &bundle,
            options(Some(&science), false, false),
            &context
        )
        .await,
        Err(Error::Refused(_))
    ));
    sqlx::query("DELETE FROM users WHERE issuer='fixture' AND subject='duplicate'")
        .execute(&pool)
        .await?;
    assert_eq!(snapshot(&pool).await?, before);
    assert!(matches!(
        run_import(&pool, &bundle, options(None, false, false), &context).await,
        Err(Error::Refused(_))
    ));
    assert_eq!(snapshot(&pool).await?, before);
    let dry = run_import(
        &pool,
        &bundle,
        options(Some(&science), true, false),
        &context,
    )
    .await?;
    assert_eq!(
        (
            dry.policies,
            dry.tracks,
            dry.hypotheses,
            dry.attempts,
            dry.decisions,
            dry.reports
        ),
        (1, 2, 5, 6, 4, 1)
    );
    assert_eq!(snapshot(&pool).await?, before);
    let (first, second) = tokio::join!(
        run_import(
            &pool,
            &bundle,
            options(Some(&science), false, false),
            &context
        ),
        run_import(
            &pool,
            &bundle,
            options(Some(&science), false, false),
            &context
        )
    );
    let first = first?;
    let second = second?;
    assert_eq!(
        usize::from(first.project_created) + usize::from(second.project_created),
        1
    );
    assert!(first.nothing_new() || second.nothing_new());
    for (table, expected) in [
        ("projects", 1),
        ("memberships", 2),
        ("import_entries", 9),
        ("hypotheses", 5),
        ("attempts", 6),
        ("decisions", 4),
        ("measurements", 10),
        ("jobs", 1),
        ("uploads", 0),
    ] {
        assert_eq!(count(&pool, table).await?, expected, "{table}");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE actor_kind='system' AND via_channel='cli'"
        )
        .fetch_one(&pool)
        .await?,
        13
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM phase_outputs WHERE stage='writeup' AND origin='imported' AND body<>'' AND front_matter->>'kind'='retrospective'").fetch_one(&pool).await?,1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM hypotheses WHERE origin='imported' AND source_ref IS NOT NULL AND external_id IS NOT NULL").fetch_one(&pool).await?,5);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM measurements WHERE authority IN ('imported_artifact','imported_transcribed')").fetch_one(&pool).await?,10);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM artifacts WHERE backend='external' AND bucket='' AND origin='imported' AND job_id IS NULL").fetch_one(&pool).await?,3);
    // The hypothesis awaiting its decision is written up first: it has a
    // document job and no decision case yet.
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM review_cases WHERE origin='imported' AND subject_revision=1"
        )
        .fetch_one(&pool)
        .await?,
        4
    );
    let stored = snapshot(&pool).await?;
    let mut equivalent_science = science.clone();
    equivalent_science["hypothesis_fields"]["properties"]["top_k"]["minimum"] =
        serde_json::from_str("1e0")?;
    assert!(
        run_import(
            &pool,
            &bundle,
            options(Some(&equivalent_science), false, false),
            &context
        )
        .await?
        .nothing_new()
    );
    assert_eq!(snapshot(&pool).await?, stored);
    let replay = run_import(&pool, &bundle, options(None, false, false), &context).await?;
    assert!(replay.nothing_new());
    assert_eq!(replay.unchanged, 9);
    assert_eq!(snapshot(&pool).await?, stored);
    let scientific = Directory::new()?;
    support::copy(&directory.0, &scientific.0)?;
    let hypothesis_path = scientific.0.join("hypotheses/H-001.yaml");
    let hypothesis = bundle
        .entries()
        .find(|entry| entry.key() == "H-001")
        .ok_or("hypothesis")?;
    let mut input = hypothesis.content().clone();
    for attempt in input["attempts"].as_array_mut().ok_or("attempts array")? {
        if let Some(report) = attempt.get_mut("report").and_then(Value::as_object_mut) {
            report.remove("sha256");
        }
    }
    let rendered = serde_json::to_string(&input)?.replace("\"value\":0.71", "\"value\":71e-2");
    assert!(rendered.contains("71e-2"));
    std::fs::remove_file(&hypothesis_path)?;
    std::fs::write(hypothesis_path.with_extension("json"), rendered)?;
    let scientific = fixture(&scientific, &context)?;
    assert!(
        run_import(&pool, &scientific, options(None, false, false), &context)
            .await?
            .nothing_new()
    );
    assert_eq!(snapshot(&pool).await?, stored);
    let project_path = directory.0.join("project.yaml");
    let project = std::fs::read_to_string(&project_path)?;
    std::fs::write(
        &project_path,
        project.replace("Retrieval research history", "Changed immutable title"),
    )?;
    let changed = fixture(&directory, &context)?;
    let result = run_import(&pool, &changed, options(None, false, false), &context).await;
    assert!(
        matches!(result,Err(Error::Refused(ref problems)) if problems.iter().any(|p|p.pointer=="/title"))
    );
    assert_eq!(snapshot(&pool).await?, stored);
    std::fs::write(project_path, project)?;
    let policy_path = directory.0.join("policies/historical-evaluator.yaml");
    // A missing entry is retained only with the explicit flag.
    let policy_path = if policy_path.exists() {
        policy_path
    } else {
        std::fs::read_dir(directory.0.join("policies"))?
            .next()
            .ok_or("policy")??
            .path()
    };
    let policy = std::fs::read(&policy_path)?;
    std::fs::remove_file(&policy_path)?;
    let missing = fixture(&directory, &context)?;
    assert!(matches!(
        run_import(&pool, &missing, options(None, false, false), &context).await,
        Err(Error::Refused(_))
    ));
    let accepted = run_import(&pool, &missing, options(None, false, true), &context).await?;
    assert_eq!(accepted.missing.len(), 1);
    assert_eq!(snapshot(&pool).await?, stored);
    std::fs::write(policy_path, policy)?;
    // Existing deciders require a researcher membership, even when history is otherwise unchanged.
    sqlx::query("UPDATE memberships SET role='viewer' WHERE user_id=(SELECT id FROM users WHERE subject='ben')").execute(&pool).await?;
    let membership_snapshot = snapshot(&pool).await?;
    let extra = directory.0.join("hypotheses/added.json");
    std::fs::write(
        &extra,
        r#"{"id":"added","track":"lexical","title":"New historical question","kind":"experiment","claim":"Useful claim","sources":["notebook.md:1@abc"],"created_at":"2025-03-01","state":"failed","attempts":[{"label":"run-1","started_at":"2025-03-01","finished_at":"2025-03-01","status":"failed","failure":{"code":"out_of_memory","reason":"The run ran out of memory."}}],"decision":{"action":"close_failed","reason":"Closed in notebook","decided_by":"ben@example.org","decided_at":"2025-03-02","source":"notebook.md:1@abc"}}"#,
    )?;
    let added = fixture(&directory, &context)?;
    assert!(matches!(
        run_import(&pool, &added, options(None, false, false), &context).await,
        Err(Error::Refused(_))
    ));
    assert_eq!(snapshot(&pool).await?, membership_snapshot);
    sqlx::query("UPDATE memberships SET role='researcher' WHERE user_id=(SELECT id FROM users WHERE subject='ben')").execute(&pool).await?;
    let normal: Value = serde_json::from_slice(&std::fs::read(&extra)?)?;
    let semantic_before = snapshot(&pool).await?;
    for (key, value) in [
        ("track", serde_json::json!("missing-track")),
        (
            "control",
            serde_json::json!({"hypothesis":"missing-hypothesis"}),
        ),
    ] {
        let mut invalid = normal.clone();
        invalid[key] = value;
        std::fs::write(&extra, serde_json::to_vec(&invalid)?)?;
        let invalid = fixture(&directory, &context)?;
        assert!(matches!(
            run_import(&pool, &invalid, options(None, false, false), &context).await,
            Err(Error::Refused(_))
        ));
        assert_eq!(snapshot(&pool).await?, semantic_before);
    }
    let mut invalid = normal.clone();
    invalid["decision"]["decided_at"] = serde_json::json!("2025-01-01");
    std::fs::write(&extra, serde_json::to_vec(&invalid)?)?;
    assert!(matches!(
        run_import(
            &pool,
            &fixture(&directory, &context)?,
            options(None, false, false),
            &context
        )
        .await,
        Err(Error::Refused(_))
    ));
    assert_eq!(snapshot(&pool).await?, semantic_before);
    std::fs::write(&extra, serde_json::to_vec(&normal)?)?;
    // A late database fault rolls back history, counters, ledger and audit together.
    sqlx::raw_sql("CREATE FUNCTION import_fault() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN IF NEW.action='import.completed' THEN RAISE EXCEPTION 'fixture fault'; END IF; RETURN NEW; END$$; CREATE TRIGGER import_fault BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION import_fault()").execute(&pool).await?;
    let fault_before = snapshot(&pool).await?;
    assert!(matches!(
        run_import(&pool, &added, options(None, false, false), &context).await,
        Err(Error::Database { .. })
    ));
    assert_eq!(snapshot(&pool).await?, fault_before);
    sqlx::raw_sql("DROP TRIGGER import_fault ON audit_events; DROP FUNCTION import_fault()")
        .execute(&pool)
        .await?;
    let appended = run_import(&pool, &added, options(None, false, false), &context).await?;
    assert_eq!((appended.hypotheses, appended.decisions), (1, 1));
    assert_eq!(count(&pool, "hypotheses").await?, 6);
    // Imported records remain append-only under the real migration triggers.
    let refused = sqlx::query("UPDATE import_entries SET sha256='changed'")
        .execute(&pool)
        .await
        .err()
        .ok_or("append-only trigger accepted an update")?;
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("P0001")
    );
    // Cancellation while waiting for the advisory lock must release its transaction/connection.
    let mut blocker = pool.begin().await?;
    let blocker_pid = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('cannery import retrieval-history',0))",
    )
    .execute(&mut *blocker)
    .await?;
    let cancellation_before = snapshot(&pool).await?;
    let task_pool = pool.clone();
    let task_bundle = added.clone();
    let task_context = support::context()?;
    let task = tokio::spawn(async move {
        run_import(
            &task_pool,
            &task_bundle,
            options(None, false, false),
            &task_context,
        )
        .await
    });
    for _ in 0..100 {
        let waiting=sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'").fetch_one(&pool).await?;
        if waiting > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory'").fetch_one(&pool).await?>0);
    task.abort();
    assert!(task.await.is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let unsettled = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND pid<>$1 AND (state='idle in transaction' OR wait_event='advisory')",
            ).bind(blocker_pid).fetch_one(&pool).await?;
            if unsettled == 0 {
                break Ok::<(),sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    blocker.rollback().await?;
    assert_eq!(snapshot(&pool).await?, cancellation_before);
    assert!(
        run_import(&pool, &added, options(None, false, false), &context)
            .await?
            .nothing_new()
    );
    pool.close().await;
    Ok(())
}
