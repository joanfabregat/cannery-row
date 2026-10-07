#![forbid(unsafe_code)]
use conformance::{Coverage, Result, requirements::Requirements};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let requirements = arguments.next().ok_or(
        "usage: coverage REQUIREMENTS_JSON SOURCE_REFERENCE_JSON OUTPUT_JSON REPORT_JSON...",
    )?;
    let reference = arguments.next().ok_or("source reference path required")?;
    let output = arguments.next().ok_or("combined report path required")?;
    let expected: Requirements = serde_json::from_slice(&std::fs::read(requirements)?)?;
    expected.check_reference(&serde_json::from_slice(&std::fs::read(reference)?)?)?;
    let expected = expected.coverage()?;
    let mut observed = Coverage::default();
    let mut report_count = 0;
    for path in arguments {
        let report: Coverage = serde_json::from_slice(&std::fs::read(path)?)?;
        observed.merge(&report);
        report_count += 1;
    }
    if report_count == 0 {
        return Err("at least one observed coverage report is required".into());
    }
    std::fs::write(output, serde_json::to_vec_pretty(&observed)?)?;
    let missing = observed.missing(&expected);
    if !missing.is_empty() {
        return Err(format!("missing conformance coverage:\n{}", missing.join("\n")).into());
    }
    println!(
        "coverage complete: {} operations, {} tools, {} audit actions",
        expected.operations.len(),
        expected.tools.len(),
        expected.audit_actions.len()
    );
    Ok(())
}
