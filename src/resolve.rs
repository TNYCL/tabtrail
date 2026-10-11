//! Executable resolution and identity, without running anything.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::schema::ExecutableIdentity;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The name the user typed (used as argv[0] for probes).
    pub name: String,
    pub identity: ExecutableIdentity,
}

/// Paths that cannot be represented losslessly and safely as one text line
/// (invalid UTF-8, control characters such as CR/LF, bidi overrides) are
/// not supported: they could inject entries into line-based files such as
/// the trust list or manipulate the terminal when displayed.
pub fn safe_path_text(p: &Path) -> Option<&str> {
    let s = p.to_str()?;
    (!s.chars().any(crate::sanitize::is_forbidden_char)).then_some(s)
}

fn is_executable_file(p: &Path) -> Option<fs::Metadata> {
    let m = fs::metadata(p).ok()?;
    (m.is_file() && m.permissions().mode() & 0o111 != 0).then_some(m)
}

/// Resolves `command` the way the shell would for an external command:
/// names containing `/` are paths relative to `cwd`; other names are looked
/// up in `path_var` in order (an empty entry means the current directory).
pub fn resolve(command: &str, path_var: Option<&OsStr>, cwd: &Path) -> Option<Resolved> {
    if command.is_empty() || command.contains('\0') {
        return None;
    }
    let candidate = if command.contains('/') {
        let p = Path::new(command);
        let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
        is_executable_file(&full).map(|m| (full, m))
    } else {
        let path_var = path_var?;
        std::env::split_paths(path_var).find_map(|dir| {
            let dir = if dir.as_os_str().is_empty() {
                cwd.to_path_buf()
            } else {
                dir
            };
            let dir = if dir.is_absolute() { dir } else { cwd.join(dir) };
            let full: PathBuf = dir.join(command);
            is_executable_file(&full).map(|m| (full, m))
        })
    }?;
    let (path, meta) = candidate;
    let canonical = fs::canonicalize(&path).ok()?;
    let path_text = safe_path_text(&path)?.to_string();
    let canonical_text = safe_path_text(&canonical)?.to_string();
    let name = command.rsplit('/').next().unwrap_or(command);
    if name.chars().any(crate::sanitize::is_forbidden_char) {
        return None;
    }
    Some(Resolved {
        name: name.to_string(),
        identity: ExecutableIdentity {
            path: path_text,
            canonical: canonical_text,
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        },
    })
}

/// Re-reads the identity of an already resolved path (None if it vanished).
pub fn current_identity(path: &str) -> Option<ExecutableIdentity> {
    let p = Path::new(path);
    safe_path_text(p)?;
    let meta = is_executable_file(p)?;
    let canonical = fs::canonicalize(p).ok()?;
    Some(ExecutableIdentity {
        path: path.to_string(),
        canonical: safe_path_text(&canonical)?.to_string(),
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, write_script};

    #[test]
    fn resolves_in_path_order_and_explicit_paths() {
        let t = TempDir::new("resolve");
        let a = t.path().join("a");
        let b = t.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        write_script(&a.join("tool"), "echo a");
        write_script(&b.join("tool"), "echo b");
        fs::write(b.join("plain"), "not executable").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        let r = resolve("tool", Some(&path), t.path()).unwrap();
        assert_eq!(r.identity.path, a.join("tool").to_string_lossy());
        let path_rev = std::env::join_paths([&b, &a]).unwrap();
        let r2 = resolve("tool", Some(&path_rev), t.path()).unwrap();
        assert_ne!(r.identity, r2.identity, "PATH order changes identity");
        assert!(resolve("plain", Some(&path), t.path()).is_none());
        assert!(resolve("missing", Some(&path), t.path()).is_none());
        let r3 = resolve("./a/tool", None, t.path()).unwrap();
        assert_eq!(r3.name, "tool");
        assert_eq!(r3.identity.canonical, r.identity.canonical);
    }

    #[test]
    fn unsafe_paths_are_not_resolved() {
        use std::os::unix::ffi::OsStrExt;
        let t = TempDir::new("resolve-unsafe");
        // a directory whose name embeds a newline followed by another path
        let evil_dir = t.path().join("trusted\n/usr/bin");
        fs::create_dir_all(&evil_dir).unwrap();
        write_script(&evil_dir.join("env"), "true");
        let evil = evil_dir.join("env");
        assert!(resolve(evil.to_str().unwrap(), None, t.path()).is_none());
        assert!(current_identity(evil.to_str().unwrap()).is_none());
        // and via PATH lookup
        assert!(resolve("env", Some(evil_dir.as_os_str()), t.path()).is_none());
        // invalid UTF-8 directory name
        let bad = t.path().join(std::ffi::OsStr::from_bytes(b"bin\xff"));
        fs::create_dir_all(&bad).unwrap();
        write_script(&bad.join("tool"), "true");
        assert!(resolve("tool", Some(bad.as_os_str()), t.path()).is_none());
        // a symlink with a clean name pointing at an unsafe target
        let link = t.path().join("clean");
        std::os::unix::fs::symlink(&evil, &link).unwrap();
        assert!(resolve(link.to_str().unwrap(), None, t.path()).is_none());
    }

    #[test]
    fn identity_changes_when_file_is_replaced() {
        let t = TempDir::new("resolve2");
        let p = t.path().join("tool");
        write_script(&p, "echo 1");
        let before = current_identity(p.to_str().unwrap()).unwrap();
        let tmp = t.path().join("tool.new");
        write_script(&tmp, "echo 2 longer");
        fs::rename(&tmp, &p).unwrap();
        let after = current_identity(p.to_str().unwrap()).unwrap();
        assert_ne!(before, after);
    }
}
