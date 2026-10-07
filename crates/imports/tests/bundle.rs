#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
mod support;
use cannery_imports::{canonical_sha256, read_bundle};
use serde_json::json;
use support::{Directory, LIMITS, Result};
#[test]
fn reviewed_yaml_json_reports_and_digest() -> Result {
    let context = support::context()?;
    let original = read_bundle(
        &support::root().join("examples/import"),
        LIMITS,
        &context.contracts,
    )?;
    let reference: serde_json::Value = serde_json::from_str(runtime_reference!(
        "/../../crates/imports/tests/fixtures/example_reference.json"
    ))?;
    assert_eq!(
        original.sha256(),
        reference["bundle_sha256"].as_str().ok_or("digest")?
    );
    for expected in reference["entries"].as_array().ok_or("entries")? {
        let entry = original
            .entries()
            .find(|entry| {
                Some(entry.kind().as_str()) == expected["kind"].as_str()
                    && Some(entry.key()) == expected["key"].as_str()
            })
            .ok_or("source entry")?;
        assert_eq!(
            canonical_sha256(entry.content()),
            expected["sha256"].as_str().ok_or("entry digest")?
        );
    }
    assert_eq!(original.entries().count(), 11);
    assert_eq!(original.reports().len(), 1);
    let directory = Directory::new()?;
    for entry in original.entries() {
        let path = directory.0.join(entry.path()).with_extension("json");
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        let mut document = entry.content().clone();
        if let Some(attempts) = document
            .get_mut("attempts")
            .and_then(serde_json::Value::as_array_mut)
        {
            for attempt in attempts {
                if let Some(report) = attempt
                    .get_mut("report")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    report.remove("sha256");
                }
            }
        }
        std::fs::write(path, serde_json::to_vec(&document)?)?;
    }
    for (path, body) in original.reports() {
        let path = directory.0.join(path);
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        std::fs::write(path, body)?;
    }
    let converted = read_bundle(&directory.0, LIMITS, &context.contracts)?;
    assert_eq!(converted.sha256(), original.sha256());
    assert_eq!(
        canonical_sha256(&json!({"a":1,"b":2})),
        canonical_sha256(&json!({"b":2,"a":1}))
    );
    Ok(())
}
#[test]
fn parser_and_file_security_limits() -> Result {
    let context = support::context()?;
    for text in [
        "slug: alpha\nslug: beta\ntitle: Some title\ncreated_by: ana@example.org\n",
        "{\"slug\":\"alpha\",\"slug\":\"beta\",\"title\":\"Some title\",\"created_by\":\"ana@example.org\"}",
        "slug: alpha\ntitle: &id Some title\ndescription: *id\ncreated_by: ana@example.org\n",
        "slug: alpha\ntitle: !!python/object unsafe\n",
        "slug: alpha\ntitle: Some title\ndescription: .nan\ncreated_by: ana@example.org\n",
        "{\"slug\":\"alpha\",\"title\":\"Some title\",\"description\":\"\\u0000\",\"created_by\":\"ana@example.org\"}",
    ] {
        let directory = Directory::new()?;
        std::fs::write(directory.0.join("project.yaml"), text)?;
        assert!(
            read_bundle(&directory.0, LIMITS, &context.contracts).is_err(),
            "unsafe document accepted"
        );
    }
    let directory = Directory::new()?;
    support::copy(&support::root().join("examples/import"), &directory.0)?;
    let original = read_bundle(&directory.0, LIMITS, &context.contracts)?;
    let report_path = directory
        .0
        .join(original.reports().keys().next().ok_or("report")?);
    let body = std::fs::read(&report_path)?;
    for bytes in [&b"\xff"[..], &b"\0"[..], &b" "[..]] {
        std::fs::write(&report_path, bytes)?;
        assert!(read_bundle(&directory.0, LIMITS, &context.contracts).is_err());
    }
    std::fs::write(report_path, body)?;
    assert!(
        read_bundle(
            &directory.0,
            cannery_imports::BundleLimits {
                max_report_bytes: 1,
                ..LIMITS
            },
            &context.contracts
        )
        .is_err()
    );
    for limits in [
        BundleLimitsOverride::files(),
        BundleLimitsOverride::bytes(),
        BundleLimitsOverride::depth(),
        BundleLimitsOverride::nodes(),
    ] {
        assert!(read_bundle(&directory.0, limits, &context.contracts).is_err());
    }
    std::os::unix::fs::symlink("/etc/passwd", directory.0.join("reports/escape.md"))?;
    assert!(read_bundle(&directory.0, LIMITS, &context.contracts).is_err());
    std::fs::remove_file(directory.0.join("reports/escape.md"))?;
    std::fs::write(directory.0.join("reports/orphan.md"), "unreferenced")?;
    assert!(read_bundle(&directory.0, LIMITS, &context.contracts).is_err());
    Ok(())
}
struct BundleLimitsOverride;
impl BundleLimitsOverride {
    fn files() -> cannery_imports::BundleLimits {
        cannery_imports::BundleLimits {
            max_files: 1,
            ..LIMITS
        }
    }
    fn bytes() -> cannery_imports::BundleLimits {
        cannery_imports::BundleLimits {
            max_total_bytes: 10,
            ..LIMITS
        }
    }
    fn depth() -> cannery_imports::BundleLimits {
        cannery_imports::BundleLimits {
            max_depth: 2,
            ..LIMITS
        }
    }
    fn nodes() -> cannery_imports::BundleLimits {
        cannery_imports::BundleLimits {
            max_nodes: 2,
            ..LIMITS
        }
    }
}
