//! Owner-only permissions for the data directory and every file in it.
//!
//! On Unix the directory is 0700 and files are 0600. On Windows the per-user
//! `%APPDATA%` ACL already restricts access to the signed-in account.

use std::fs;
use std::io::Write;
use std::path::Path;

pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Restrict an existing file to the owner. Missing files are ignored.
pub fn restrict_file(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Restrict a SQLite database and its WAL/SHM side files.
pub fn restrict_db_files(db_path: &Path) -> std::io::Result<()> {
    restrict_file(db_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(suffix);
        restrict_file(Path::new(&p))?;
    }
    Ok(())
}

/// Write a file that only the owner can read, atomically (temp + rename).
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    restrict_file(path)
}

/// Make the directory entries of `dir` durable (a rename into it survives
/// a power cut). Best effort: Unix only (Windows has no directory handle to
/// sync), and a file system that refuses it is not an error.
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(e) = fs::File::open(dir).and_then(|f| f.sync_all()) {
        tracing::debug!("Could not sync the data folder: {}", e);
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn dir_and_file_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let f = dir.join("vault.json");
        write_private_file(&f, b"{}").unwrap();
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let db = dir.join("x.db");
        fs::write(&db, b"").unwrap();
        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        restrict_db_files(&db).unwrap();
        assert_eq!(
            fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
