use super::{
    fetch,
    runner::{COMMIT_A, REPOSITORY, Rig, cache_science, cached_producer},
};
use conformance::Result;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub async fn rig(label: &str, pause: bool) -> Result<(Rig, fetch::Mock, Value)> {
    let mut producer = cached_producer(COMMIT_A)?;
    producer["spec"]["container"]["env"]
        .as_array_mut()
        .ok_or("env missing")?
        .push(json!({"name":"CACHE_VARIANT","value":"A"}));
    if pause {
        producer["spec"]["container"]["command"] = json!(["python3", "cache_control.py"]);
    }
    let mut rig = Rig::new(label, producer.clone(), cache_science()?).await?;
    let root = rig
        .work
        .0
        .to_str()
        .ok_or("owned root is not UTF8")?
        .to_owned();
    producer["spec"]["setup"]["run"] = json!(format!(
        "mkdir -p \"$CR_ROOT/cache/site\" && cat requirements.txt > \"$CR_ROOT/cache/site/fixture.txt\" && dd if=/dev/zero of=\"$CR_ROOT/cache/site/blob\" bs=4096 count=1 2>/dev/null && printf '%s' \"$CACHE_VARIANT\" > \"$CR_ROOT/cache/site/variant\" && printf x >> '{}'",
        root.replace('\'', "'\\''") + "/setup-count"
    ));
    rig.bind(producer.clone()).await?;
    if pause {
        rig.script(
            "cache_control.py",
            &CONTROL.replace("OWNED_ROOT", &serde_json::to_string(&root)?),
        )?;
    }
    let (archive, public) = fetch::archive(&rig, "valid", false).await?;
    let mock = fetch::Mock::new("plain", archive, public).await?;
    mock.configure(&mut rig, false)?;
    Ok((rig, mock, producer))
}
const CONTROL: &str = r"import runpy
import time
from pathlib import Path

def main() -> None:
    owned = Path(OWNED_ROOT)
    (owned / 'producer-started').write_text('started')
    until = time.monotonic() + 30
    while not (owned / 'producer-release').exists():
        if time.monotonic() > until:
            raise RuntimeError('controlled producer wait exceeded bound')
        time.sleep(0.05)
    runpy.run_path('produce_overlap.py', run_name='__main__')

if __name__ == '__main__':
    main()
";
pub async fn wait_started(rig: &Rig) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(20);
    while !rig.work.0.join("producer-started").exists() {
        if Instant::now() > until {
            return Err("controlled producer did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Ok(())
}
pub fn release(rig: &Rig) -> Result<()> {
    fs::write(rig.work.0.join("producer-release"), "release")?;
    Ok(())
}
pub fn code(rig: &Rig) -> PathBuf {
    rig.work
        .0
        .join("cache/code")
        .join(REPOSITORY)
        .join(COMMIT_A)
}
pub fn setups(rig: &Rig) -> Result<Vec<PathBuf>> {
    let root = rig.work.0.join("cache/setup");
    if !root.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?)
}
pub fn count(rig: &Rig) -> Result<usize> {
    Ok(fs::read(rig.work.0.join("setup-count"))?.len())
}
pub fn variant_entry(rig: &Rig, name: &str) -> Result<PathBuf> {
    for entry in setups(rig)? {
        if fs::read_to_string(entry.join("site/variant"))? == name {
            return Ok(entry);
        }
    }
    Err("expected published variant missing".into())
}
pub fn bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if metadata.is_dir() {
            total += bytes(&entry.path())?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}
pub async fn complete(rig: &mut Rig, mock: &fetch::Mock) -> Result<i64> {
    let number = rig.submit().await?;
    let output = rig.run().await?;
    super::runner::successful_process(&output);
    let job = rig.job(number).await?;
    assert_eq!(job["state"], "completed");
    fetch::assert_redacted(rig, &output, &job);
    mock.assert_credentials();
    rig.assert_clean()?;
    Ok(number)
}
