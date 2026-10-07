#![forbid(unsafe_code)]
use cannery_runner::paths::{PathError, from_text, resolve, resolve_text};
use std::{
    error::Error,
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
    path::PathBuf,
};
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
struct Owned(PathBuf);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn native_paths_resolve_links_missing_destinations_and_opaque_bytes() -> Result {
    let root = std::env::temp_dir().join(format!("cannery-native-paths-{}", std::process::id()));
    fs::create_dir(&root)?;
    let owned = Owned(root);
    let root = fs::canonicalize(&owned.0)?;
    fs::create_dir(root.join("real"))?;
    fs::write(root.join("real/file"), b"fixture")?;
    symlink("real", root.join("link"))?;
    assert_eq!(
        resolve(&root.join("link/file"), true)?,
        root.join("real/file")
    );
    assert_eq!(
        resolve(&root.join("link/missing/child"), false)?,
        root.join("real/missing/child")
    );
    assert_eq!(
        resolve(&root.join("link/missing/../new"), false)?,
        root.join("real/new")
    );
    assert!(resolve(&root.join("missing"), true).is_err());
    let raw = root.join(OsString::from_vec(vec![b'f', 0xff]));
    fs::write(&raw, b"opaque")?;
    assert_eq!(resolve(&raw, true)?, raw);
    let text = root.join("é");
    fs::write(&text, b"utf8")?;
    assert_eq!(resolve_text(text.to_str().ok_or("UTF8 root")?, true)?, text);
    assert_eq!(from_text("é")?, PathBuf::from("é"));
    assert_eq!(from_text("before\0after"), Err(PathError::InvalidNul));
    assert_eq!(
        resolve(&PathBuf::from(OsString::from_vec(vec![0xff, 0])), false),
        Err(PathError::InvalidNul)
    );
    assert_eq!(
        resolve(&root.join("real/file/child"), false),
        Err(PathError::NotDirectory)
    );
    symlink("cycle", root.join("cycle"))?;
    for strict in [false, true] {
        assert!(matches!(
            resolve(&root.join("cycle"), strict),
            Err(PathError::Io { .. })
        ));
    }
    for number in 0..80 {
        let target = if number == 79 {
            "real".to_owned()
        } else {
            format!("hop{}", number + 1)
        };
        symlink(target, root.join(format!("hop{number}")))?;
    }
    assert!(resolve(&root.join("hop0"), true).is_err());
    symlink("absent", root.join("dangling"))?;
    assert_eq!(
        resolve(&root.join("dangling"), false)?,
        root.join("dangling")
    );
    Ok(())
}
