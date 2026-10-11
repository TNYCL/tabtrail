//! Per-user schema cache under `$XDG_CACHE_HOME/tabtrail/v1`.
//!
//! * Entries are keyed by the resolved executable path and validated against
//!   the full executable identity (device, inode, size, mtime), the schema
//!   version, the parser version and the collection locale. Anything else is
//!   treated as stale and never used.
//! * Writes are atomic (temp file + rename); readers never see partial data.
//! * Corrupt entries are ignored and removed.
//! * The number of entries and the size of each entry are bounded.
//!
//! Only command metadata is stored: no command lines, history, environment
//! values or provider results.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::config::Dirs;
use crate::fsutil::{create_private_dir, fnv1a64, read_regular, write_atomic};
use crate::help::PARSER_VERSION;
use crate::schema::{CollectionPolicy, ExecutableIdentity, SCHEMA_VERSION, SchemaDocument};

pub const MAX_ENTRIES: usize = 512;
pub const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
/// Locale under which metadata is collected (probes force LC_ALL=C).
pub const COLLECTION_LOCALE: &str = "C";

#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    Fresh(Box<SchemaDocument>),
    /// An entry exists but describes a different executable or format.
    Stale,
    Missing,
    /// Unreadable or malformed; it has been removed.
    Corrupt,
}

#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(dirs: &Dirs) -> Cache {
        Cache {
            root: dirs.cache.join("v1"),
        }
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn key(identity: &ExecutableIdentity) -> String {
        format!("{:016x}", fnv1a64(identity.path.as_bytes()))
    }

    fn schema_path(&self, identity: &ExecutableIdentity) -> PathBuf {
        self.root.join("schemas").join(format!("{}.json", Self::key(identity)))
    }

    /// `man_roots` is the current manual page root fingerprint (see
    /// `CollectionPolicy::man_roots`); entries built from other roots, or
    /// from pages that have since changed, are stale.
    pub fn load(&self, identity: &ExecutableIdentity, man_roots: u64) -> Lookup {
        let path = self.schema_path(identity);
        // Never blocks (FIFOs etc. are rejected) and never reads unbounded.
        let data = match read_regular(&path, MAX_ENTRY_BYTES) {
            Ok(Some(d)) => d,
            Ok(None) => return Lookup::Missing,
            Err(_) => {
                let _ = fs::remove_file(&path);
                return Lookup::Corrupt;
            }
        };
        match serde_json::from_slice::<SchemaDocument>(&data) {
            Ok(doc) => {
                if doc.schema_version == SCHEMA_VERSION
                    && doc.parser_version == PARSER_VERSION
                    && doc.locale == COLLECTION_LOCALE
                    && doc.executable == *identity
                    && doc.collected_with.man_roots == man_roots
                    && man_sources_unchanged(&doc)
                {
                    Lookup::Fresh(Box::new(doc))
                } else {
                    Lookup::Stale
                }
            }
            Err(_) => {
                let _ = fs::remove_file(&path);
                Lookup::Corrupt
            }
        }
    }

    pub fn store(&self, doc: &SchemaDocument) -> io::Result<()> {
        let data = serde_json::to_vec(doc).map_err(io::Error::other)?;
        if data.len() as u64 > MAX_ENTRY_BYTES {
            return Err(io::Error::other("schema entry too large"));
        }
        write_atomic(&self.schema_path(&doc.executable), &data)?;
        self.prune(MAX_ENTRIES);
        Ok(())
    }

    /// Keeps at most `max` schema entries (oldest removed first) and removes
    /// temporary files left by interrupted writers.
    pub fn prune(&self, max: usize) {
        let dir = self.root.join("schemas");
        let Ok(rd) = fs::read_dir(&dir) else { return };
        let mut entries = Vec::new();
        let now = SystemTime::now();
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if name.ends_with(".tmp") {
                if now.duration_since(mtime).unwrap_or_default() > Duration::from_secs(3600) {
                    let _ = fs::remove_file(e.path());
                }
                continue;
            }
            if name.ends_with(".json") {
                entries.push((mtime, e.path()));
            }
        }
        if entries.len() > max {
            entries.sort();
            let excess = entries.len() - max;
            for (_, p) in entries.into_iter().take(excess) {
                let _ = fs::remove_file(p);
            }
        }
    }

    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_dir_all(&self.root) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn marker_path(&self, kind: Marker, identity: &ExecutableIdentity, sub: &[String]) -> PathBuf {
        let mut key = identity.path.clone();
        for s in sub {
            key.push('\0');
            key.push_str(s);
        }
        self.root
            .join(kind.dir())
            .join(format!("{:016x}", fnv1a64(key.as_bytes())))
    }

    fn fingerprint(identity: &ExecutableIdentity, policy: CollectionPolicy) -> String {
        format!(
            "{}:{}:{}:{}:{}{}:{:x}",
            identity.dev,
            identity.ino,
            identity.size,
            identity.mtime_ns,
            policy.help as u8,
            policy.man as u8,
            policy.man_roots
        )
    }

    /// True if a marker of `kind` exists for this executable identity,
    /// subcommand path and policy, younger than `within`.
    pub fn marker_recent(
        &self,
        kind: Marker,
        identity: &ExecutableIdentity,
        sub: &[String],
        policy: CollectionPolicy,
        within: Duration,
    ) -> bool {
        let p = self.marker_path(kind, identity, sub);
        let Ok(meta) = fs::symlink_metadata(&p) else {
            return false;
        };
        let fresh = meta.is_file()
            && meta
                .modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age < within);
        fresh && matches!(read_regular(&p, 256), Ok(Some(d)) if d == Self::fingerprint(identity, policy).as_bytes())
    }

    pub fn record_marker(&self, kind: Marker, identity: &ExecutableIdentity, sub: &[String], policy: CollectionPolicy) {
        let _ = write_atomic(
            &self.marker_path(kind, identity, sub),
            Self::fingerprint(identity, policy).as_bytes(),
        );
        prune_dir(&self.root.join(kind.dir()), MAX_MARKERS);
    }

    /// Takes a non-blocking exclusive lock on one of a fixed set of lock
    /// files; released when the returned file is dropped (also when the
    /// process dies). Lock files are never deleted, so the set is bounded by
    /// construction rather than by pruning (unlinking a held lock would
    /// break mutual exclusion).
    pub fn try_lock(&self, lock: Lock) -> Option<File> {
        let dir = self.root.join("locks");
        create_private_dir(&dir).ok()?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(lock.file_name()))
            .ok()?;
        f.try_lock().ok()?;
        Some(f)
    }
}

fn man_sources_unchanged(doc: &SchemaDocument) -> bool {
    let mut sources = Vec::new();
    CollectionPolicy::man_sources(&doc.root, &mut sources);
    sources
        .iter()
        .all(|(path, id)| crate::man::file_identity(std::path::Path::new(path)).as_deref() == Some(id.as_str()))
}

/// Bounded marker namespaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// A worker actually started discovery (long cooldown).
    Attempt,
    /// The foreground spawned a worker (short throttle against storms).
    Spawn,
}

impl Marker {
    fn dir(self) -> &'static str {
        match self {
            Marker::Attempt => "attempts",
            Marker::Spawn => "spawns",
        }
    }
}

pub const MAX_MARKERS: usize = MAX_ENTRIES * 4;
/// Number of job lock stripes; executables hash onto them.
pub const JOB_LOCK_STRIPES: u64 = 16;

/// The fixed, bounded set of lock files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    WorkerSlot(usize),
    /// Job stripe for an executable path (see `Lock::job`).
    Job(u64),
}

impl Lock {
    pub fn job(identity: &ExecutableIdentity) -> Lock {
        Lock::Job(fnv1a64(identity.path.as_bytes()) % JOB_LOCK_STRIPES)
    }

    fn file_name(self) -> String {
        match self {
            Lock::WorkerSlot(i) => format!("worker-slot-{i}"),
            Lock::Job(i) => format!("job-{:02}", i % JOB_LOCK_STRIPES),
        }
    }
}

fn prune_dir(dir: &std::path::Path, max: usize) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if entries.len() > max {
        entries.sort();
        let excess = entries.len() - max;
        for (_, p) in entries.into_iter().take(excess) {
            let _ = fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::CommandNode;
    use crate::testutil::TempDir;

    fn dirs(t: &TempDir) -> Dirs {
        Dirs {
            config: t.path().join("config"),
            cache: t.path().join("cache"),
            data: t.path().join("data"),
        }
    }

    fn ident(path: &str, size: u64) -> ExecutableIdentity {
        ExecutableIdentity {
            path: path.into(),
            canonical: path.into(),
            dev: 1,
            ino: 2,
            size,
            mtime_ns: 3,
        }
    }

    fn doc(id: ExecutableIdentity) -> SchemaDocument {
        SchemaDocument {
            schema_version: SCHEMA_VERSION,
            parser_version: PARSER_VERSION,
            executable: id,
            locale: COLLECTION_LOCALE.into(),
            collected_at_unix: 0,
            collected_with: Default::default(),
            root: CommandNode {
                name: "x".into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn store_load_and_invalidation() {
        let t = TempDir::new("cache");
        let c = Cache::new(&dirs(&t));
        let id = ident("/bin/x", 10);
        assert_eq!(c.load(&id, 0), Lookup::Missing);
        c.store(&doc(id.clone())).unwrap();
        assert!(matches!(c.load(&id, 0), Lookup::Fresh(_)));
        // replaced executable (different size) -> stale, never used
        assert_eq!(c.load(&ident("/bin/x", 11), 0), Lookup::Stale);
        // different PATH resolution -> different entry
        assert_eq!(c.load(&ident("/usr/local/bin/x", 10), 0), Lookup::Missing);
        // old parser version -> stale
        let mut old = doc(id.clone());
        old.parser_version = 0;
        c.store(&old).unwrap();
        assert_eq!(c.load(&id, 0), Lookup::Stale);
    }

    #[test]
    fn corrupt_entries_fail_safely_and_are_removed() {
        let t = TempDir::new("cache-corrupt");
        let c = Cache::new(&dirs(&t));
        let id = ident("/bin/y", 1);
        c.store(&doc(id.clone())).unwrap();
        let p = c.schema_path(&id);
        fs::write(&p, b"{ not json").unwrap();
        assert_eq!(c.load(&id, 0), Lookup::Corrupt);
        assert!(!p.exists());
        fs::write(&p, vec![b' '; (MAX_ENTRY_BYTES + 1) as usize]).unwrap();
        assert_eq!(c.load(&id, 0), Lookup::Corrupt);
    }

    #[test]
    fn growth_is_bounded_and_temp_files_ignored() {
        let t = TempDir::new("cache-bound");
        let c = Cache::new(&dirs(&t));
        for i in 0..20 {
            c.store(&doc(ident(&format!("/bin/t{i}"), 1))).unwrap();
        }
        fs::write(c.root.join("schemas/.junk.1.2.tmp"), b"partial").unwrap();
        c.prune(5);
        let n = fs::read_dir(c.root.join("schemas"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .count();
        assert_eq!(n, 5);
        // a fresh temp file is kept (a writer may still own it) but never read
        assert!(c.root.join("schemas/.junk.1.2.tmp").exists());
    }

    #[test]
    fn attempts_are_identity_bound() {
        let t = TempDir::new("cache-attempt");
        let c = Cache::new(&dirs(&t));
        let id = ident("/bin/z", 1);
        let sub = vec!["deploy".to_string()];
        let man = CollectionPolicy {
            help: false,
            man: true,
            man_roots: 0,
        };
        let min = Duration::from_secs(60);
        let a = Marker::Attempt;
        assert!(!c.marker_recent(a, &id, &sub, man, min));
        c.record_marker(a, &id, &sub, man);
        assert!(c.marker_recent(a, &id, &sub, man, min));
        assert!(
            !c.marker_recent(Marker::Spawn, &id, &sub, man, min),
            "namespaces are separate"
        );
        assert!(!c.marker_recent(a, &id, &[], man, min));
        assert!(!c.marker_recent(a, &ident("/bin/z", 2), &sub, man, min));
        // trusting the executable widens the policy: retry immediately
        assert!(!c.marker_recent(
            a,
            &id,
            &sub,
            CollectionPolicy {
                help: true,
                man: true,
                man_roots: 0
            },
            min
        ));
    }

    #[test]
    fn locks_are_exclusive() {
        let t = TempDir::new("cache-lock");
        let c = Cache::new(&dirs(&t));
        let a = c.try_lock(Lock::WorkerSlot(0)).unwrap();
        assert!(c.try_lock(Lock::WorkerSlot(0)).is_none());
        assert!(c.try_lock(Lock::WorkerSlot(1)).is_some());
        drop(a);
        assert!(c.try_lock(Lock::WorkerSlot(0)).is_some());
    }

    #[test]
    fn lock_files_are_bounded() {
        let t = TempDir::new("cache-lock-bound");
        let c = Cache::new(&dirs(&t));
        for i in 0..2000 {
            let l = c.try_lock(Lock::job(&ident(&format!("/bin/tool{i}"), 1)));
            drop(l);
        }
        let n = fs::read_dir(c.root.join("locks")).unwrap().count();
        assert!(n as u64 <= JOB_LOCK_STRIPES, "{n} lock files");
    }

    #[test]
    fn markers_are_bounded() {
        let t = TempDir::new("cache-marker-bound");
        let c = Cache::new(&dirs(&t));
        let p = CollectionPolicy::default();
        for i in 0..(MAX_MARKERS + 50) {
            c.record_marker(Marker::Spawn, &ident(&format!("/bin/t{i}"), 1), &[], p);
        }
        assert!(fs::read_dir(c.root.join("spawns")).unwrap().count() <= MAX_MARKERS);
    }

    #[test]
    fn concurrent_writers_and_readers_never_see_partial_entries() {
        let t = TempDir::new("cache-conc");
        let c = Cache::new(&dirs(&t));
        let id = ident("/bin/c", 1);
        c.store(&doc(id.clone())).unwrap();
        std::thread::scope(|s| {
            for w in 0..4 {
                let c = c.clone();
                let id = id.clone();
                s.spawn(move || {
                    for i in 0..50 {
                        let mut d = doc(id.clone());
                        d.collected_at_unix = (w * 1000 + i) as u64;
                        d.root.description = Some("x".repeat(1000 + i * 37));
                        c.store(&d).unwrap();
                    }
                });
            }
            for _ in 0..4 {
                let c = c.clone();
                let id = id.clone();
                s.spawn(move || {
                    for _ in 0..200 {
                        match c.load(&id, 0) {
                            Lookup::Fresh(_) => {}
                            other => panic!("reader saw {other:?}"),
                        }
                    }
                });
            }
        });
    }
}
