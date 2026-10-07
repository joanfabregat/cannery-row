//! Authored checks for bounded native policy document traversal.
use cannery_core::json::{Document, DocumentBuilder, Node};
use cannery_runner::cli_depth::{self, DepthError, PolicyEntryPoint};
use std::error::Error;

fn nested(depth: usize, objects: bool) -> Result<Document, Box<dyn Error>> {
    let mut builder = DocumentBuilder::new();
    let mut root = builder.push(Node::Null)?;
    for _ in 0..depth {
        root = builder.push(if objects {
            Node::Object(vec![("child".to_owned(), root)])
        } else {
            Node::Array(vec![root])
        })?;
    }
    Ok(builder.finish(root)?)
}

#[test]
fn policy_callers_share_a_bounded_native_limit() -> Result<(), Box<dyn Error>> {
    assert_eq!(cli_depth::JSON_CONTAINERS, 128);
    for caller in [
        PolicyEntryPoint::Evaluator,
        PolicyEntryPoint::RunnerEvalKind,
    ] {
        assert_eq!(caller.validation_edges(), 128);
        for objects in [false, true] {
            for depth in [127, 128] {
                assert!(
                    cli_depth::check_validation_walk(
                        &nested(depth, objects)?,
                        caller.validation_edges()
                    )
                    .is_ok()
                );
            }
            assert_eq!(
                cli_depth::check_validation_walk(&nested(129, objects)?, caller.validation_edges()),
                Err(DepthError::Recursion)
            );
        }
    }
    Ok(())
}

#[test]
fn shared_subtrees_cannot_bypass_the_depth_limit() -> Result<(), Box<dyn Error>> {
    let mut builder = DocumentBuilder::new();
    let shared = builder.push(Node::Null)?;
    let child = builder.push(Node::Array(vec![shared]))?;
    let root = builder.push(Node::Object(vec![
        ("shallow".to_owned(), shared),
        ("deep".to_owned(), child),
    ]))?;
    assert_eq!(
        cli_depth::check_validation_walk(&builder.finish(root)?, 1),
        Err(DepthError::Recursion)
    );
    Ok(())
}

#[test]
fn scalar_root_requires_no_traversal_budget() -> Result<(), Box<dyn Error>> {
    assert!(cli_depth::check_validation_walk(&nested(0, false)?, 0).is_ok());
    assert_eq!(
        cli_depth::check_validation_walk(&nested(1, false)?, 0),
        Err(DepthError::Recursion)
    );
    Ok(())
}
