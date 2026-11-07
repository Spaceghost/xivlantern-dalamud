//! Private local sequence reservations. Crash gaps are safe; numbers are never
//! reclaimed automatically. A stale lock fails closed and needs operator review.
use std::{fs::{self, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

fn directory(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_os_string(); name.push(".announcements"); name.into()
}
fn create(path: &Path, content: &str) -> Result<(), String> {
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(path).map_err(|_| "sequence record already exists or cannot be created")?;
    file.write_all(content.as_bytes()).and_then(|_| file.sync_all()).map_err(|_| "sequence reservation could not be persisted")?;
    #[cfg(unix)] fs::File::open(path.parent().ok_or("sequence parent missing")?).and_then(|directory| directory.sync_all())
        .map_err(|_| "sequence directory could not be synced")?;
    Ok(())
}
pub fn initialize(key: &Path, public: &str, previous: u64) -> Result<(), String> {
    let dir = directory(key);
    #[cfg(unix)] {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|_| "sequence directory already exists or cannot be created")?;
    }
    #[cfg(not(unix))] fs::create_dir(&dir).map_err(|_| "sequence directory already exists or cannot be created")?;
    create(&dir.join("author"), public)?;
    create(&dir.join(format!("{previous:020}.reserved")), public)
}
struct Lock(PathBuf);
impl Drop for Lock { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
pub fn reserve(key: &Path, public: &str, seq: u64, previous: Option<u64>) -> Result<(), String> {
    if seq == 0 { return Err("announcement sequence must be positive".into()); }
    let dir = directory(key);
    if !dir.exists() {
        let last = previous.ok_or("no sequence history: use --previous-seq with the greatest sequence ever signed by this key")?;
        if seq <= last { return Err("sequence must exceed --previous-seq".into()); }
        initialize(key, public, last)?;
    } else if previous.is_some() { return Err("sequence history already exists; --previous-seq cannot override it".into()); }
    if fs::symlink_metadata(&dir).map_err(|_| "sequence directory unavailable")?.file_type().is_symlink() {
        return Err("sequence directory cannot be a symbolic link".into());
    }
    let lock_path = dir.join("lock");
    create(&lock_path, "locked")?;
    let _lock = Lock(lock_path);
    let author_path = dir.join("author");
    let mut recorded_author = String::new();
    if fs::symlink_metadata(&author_path).map_err(|_| "sequence author missing")?.file_type().is_symlink() {
        return Err("sequence author cannot be a symbolic link".into());
    }
    fs::File::open(author_path).map_err(|_| "sequence author unreadable")?.take(65)
        .read_to_string(&mut recorded_author).map_err(|_| "sequence author unreadable")?;
    if recorded_author != public {
        return Err("sequence history does not match the signing key".into());
    }
    let mut highest = None;
    for (index, entry) in fs::read_dir(&dir).map_err(|_| "sequence directory unreadable")?.enumerate() {
        if index >= 16384 { return Err("sequence history entry limit reached".into()); }
        let entry = entry.map_err(|_| "sequence entry unreadable")?;
        let name = entry.file_name().into_string().map_err(|_| "unexpected sequence entry")?;
        if name == "author" || name == "lock" { continue; }
        let digits = name.strip_suffix(".reserved").filter(|v| v.len() == 20 && v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("unexpected sequence entry; review the history before signing")?;
        if !entry.file_type().map_err(|_| "sequence entry type unreadable")?.is_file() { return Err("invalid sequence record".into()); }
        let number: u64 = digits.parse().map_err(|_| "invalid sequence number")?;
        highest = Some(highest.map_or(number, |old: u64| old.max(number)));
    }
    let highest = highest.ok_or("sequence history has no initialization record")?;
    if seq <= highest { return Err(format!("sequence must exceed the reserved sequence {highest}")); }
    create(&dir.join(format!("{seq:020}.reserved")), public)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn competing_reservations_cannot_reuse_a_number() {
        let dir = tempfile::tempdir().unwrap(); let key = dir.path().join("key");
        initialize(&key, "public", 0).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let tasks: Vec<_> = (0..2).map(|_| {
            let key = key.clone(); let barrier = barrier.clone();
            std::thread::spawn(move || { barrier.wait(); reserve(&key,"public",1,None).is_ok() })
        }).collect();
        assert_eq!(tasks.into_iter().map(|task| task.join().unwrap() as usize).sum::<usize>(),1);
        reserve(&key,"public",2,None).unwrap();
    }
    #[test]
    fn monotonic_reservations_migration_and_stale_locks_fail_closed() {
        let dir = tempfile::tempdir().unwrap(); let key = dir.path().join("key");
        assert!(reserve(&key,"public",3,None).is_err());
        reserve(&key,"public",3,Some(2)).unwrap();
        assert!(reserve(&key,"public",3,None).is_err());
        assert!(reserve(&key,"public",2,None).is_err());
        assert!(reserve(&key,"other",4,None).is_err());
        assert!(reserve(&key,"public",4,Some(0)).is_err());
        create(&directory(&key).join("lock"),"stale").unwrap();
        assert!(reserve(&key,"public",4,None).is_err());
        fs::remove_file(directory(&key).join("lock")).unwrap();
        reserve(&key,"public",4,None).unwrap();
    }
}
