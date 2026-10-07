#![forbid(unsafe_code)]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", cannery_server::generated_openapi().to_pretty_json()?);
    Ok(())
}
