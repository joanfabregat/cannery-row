//! Cancellation removes only the unpublished staging path and permits a clean retry.
#![forbid(unsafe_code)]
use cannery_storage::local::LocalStore;
use futures_util::{StreamExt, stream};
use std::{error::Error, path::PathBuf, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
struct Objects(PathBuf);
impl Drop for Objects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn cancelled_write_unlinks_its_exact_staging_file_and_retries() -> Result<()> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|_| std::io::Error::other("fixture entropy unavailable"))?;
    let directory = Objects(
        std::env::temp_dir().join(format!("cr-local-cancel-{}", uuid::Uuid::from_bytes(nonce))),
    );
    std::fs::create_dir_all(&directory.0)?;
    let store = LocalStore::new(&directory.0, "bucket")?;
    let chunks = stream::iter([Ok(b"abc".to_vec())]).chain(stream::pending());
    let mut write = Box::pin(store.write_new("data", chunks, 10));
    tokio::select! {
        result=&mut write=>{result?;return Err("write completed before cancellation".into());},
        result=tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                if let Ok(mut entries)=std::fs::read_dir(directory.0.join("bucket/.staging"))
                    && let Some(Ok(entry))=entries.next()
                    && entry.metadata()?.len()==3 {return Ok::<_,std::io::Error>(());}
                tokio::task::yield_now().await;
            }
        })=>{result??;}
    }
    drop(write);
    assert!(
        std::fs::read_dir(directory.0.join("bucket/.staging"))?
            .next()
            .is_none()
    );
    assert!(store.head("data").await?.is_none());
    let stored = store
        .write_new("data", stream::iter([Ok(b"retry".to_vec())]), 10)
        .await?;
    assert_eq!(stored.size_bytes, 5);
    assert_eq!(std::fs::read(directory.0.join("bucket/data"))?, b"retry");
    Ok(())
}
