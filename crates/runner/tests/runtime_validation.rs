//! Native pre-upload checking rejects invalid bytes without leaking values.
use cannery_runner::runtime::{
    RuntimeError, validation::NativeOutputValidator, worker::OutputValidator,
};
use serde_json::json;

#[tokio::test]
async fn registered_output_is_streamed_and_checked_before_upload()
-> Result<(), Box<dyn std::error::Error>> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)?;
    let directory = std::env::temp_dir().join(format!(
        "cannery-runtime-validator-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    ));
    std::fs::create_dir(&directory)?;
    let file = directory.join("output.json");
    let science = json!({"interfaces":[{"name":"output","version":1,"encoding":"json",
        "schema":{"type":"object","required":["count"],"properties":{"count":{"type":"integer"}}}}]});
    let validator = NativeOutputValidator::runtime_policy();
    let result = async {
        tokio::fs::write(&file, b"{\"count\":2}").await?;
        validator.check(Some("output/v1"), &science, &file).await?;
        tokio::fs::write(&file, b"{\"count\":\"private-output\"}").await?;
        assert!(matches!(
            validator.check(Some("output/v1"), &science, &file).await,
            Err(RuntimeError::InvalidStepOutput)
        ));
        tokio::fs::write(&file, b"{\"count\":NaN}").await?;
        assert!(matches!(
            validator.check(Some("output/v1"), &science, &file).await,
            Err(RuntimeError::InvalidStepOutput)
        ));
        let bounded = NativeOutputValidator {
            json_max_bytes: 4,
            context: validator.context,
        };
        tokio::fs::write(&file, b"{\"count\":2}").await?;
        assert!(matches!(
            bounded.check(Some("output/v1"), &science, &file).await,
            Err(RuntimeError::InvalidStepOutput)
        ));
        assert!(matches!(
            validator.check(Some("missing/v1"), &science, &file).await,
            Err(RuntimeError::Contract)
        ));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    std::fs::remove_dir_all(directory)?;
    result
}
