//! XDG locations, user configuration and the executable trust list.
//!
//! Configuration is a small `key = value` file so that reading it on every
//! Tab press stays cheap and dependency-free. Unknown keys are ignored.

use std::collections::BTreeSet;
use std::env;
use std::io;
use std::path::{Path, PathBuf};

/// Upper bound for the config and trust files read on every Tab.
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
}

fn xdg(var: &str, home_rel: &str) -> Option<PathBuf> {
    match env::var_os(var) {
        Some(v) if Path::new(&v).is_absolute() => Some(PathBuf::from(v)),
        _ => env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(home_rel)),
    }
}

impl Dirs {
    /// Resolves per-user directories following the XDG Base Directory spec.
    pub fn from_env() -> Option<Dirs> {
        Some(Dirs {
            config: xdg("XDG_CONFIG_HOME", ".config")?.join("tabtrail"),
            cache: xdg("XDG_CACHE_HOME", ".cache")?.join("tabtrail"),
            data: xdg("XDG_DATA_HOME", ".local/share")?.join("tabtrail"),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config")
    }

    pub fn trust_file(&self) -> PathBuf {
        self.config.join("trusted")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpProbes {
    /// Run `--help` only for executables on the trust list (default).
    Trusted,
    /// Never execute anything for discovery.
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub help_probes: HelpProbes,
    /// Read installed manual pages (no execution) for any command.
    pub man_pages: bool,
    /// Enabled dynamic value providers.
    pub providers: BTreeSet<String>,
    pub probe_timeout_ms: u64,
    pub provider_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            help_probes: HelpProbes::Trusted,
            man_pages: true,
            providers: ["git", "docker"].iter().map(|s| s.to_string()).collect(),
            probe_timeout_ms: 3000,
            provider_timeout_ms: 500,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let mut c = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "help_probes" => {
                    c.help_probes = if v == "off" {
                        HelpProbes::Off
                    } else {
                        HelpProbes::Trusted
                    };
                }
                "man_pages" => c.man_pages = !matches!(v, "off" | "false" | "no" | "0"),
                "providers" => {
                    c.providers = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty() && s != "none")
                        .collect();
                }
                "probe_timeout_ms" => {
                    if let Ok(n) = v.parse::<u64>() {
                        c.probe_timeout_ms = n.clamp(100, 30_000);
                    }
                }
                "provider_timeout_ms" => {
                    if let Ok(n) = v.parse::<u64>() {
                        c.provider_timeout_ms = n.clamp(50, 5_000);
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn load(dirs: &Dirs) -> Config {
        match crate::fsutil::read_regular(&dirs.config_file(), MAX_CONFIG_BYTES) {
            Ok(Some(t)) => Config::parse(&String::from_utf8_lossy(&t)),
            _ => Config::default(),
        }
    }
}

/// Canonical paths of executables the user allowed Tabtrail to run with a
/// help switch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustList {
    pub paths: BTreeSet<String>,
}

impl TrustList {
    pub fn parse(text: &str) -> TrustList {
        TrustList {
            paths: text
                .lines()
                .map(|l| l.trim_matches(|c| c == ' ' || c == '\t'))
                .filter(|l| Self::valid_entry(l))
                .map(String::from)
                .collect(),
        }
    }

    /// An entry is one absolute path without control or bidi characters.
    pub fn valid_entry(p: &str) -> bool {
        p.starts_with('/') && !p.chars().any(crate::sanitize::is_forbidden_char)
    }

    /// An unreadable, oversized or special (FIFO, ...) trust file trusts
    /// nothing.
    pub fn load(dirs: &Dirs) -> TrustList {
        match crate::fsutil::read_regular(&dirs.trust_file(), MAX_CONFIG_BYTES) {
            Ok(Some(t)) => TrustList::parse(&String::from_utf8_lossy(&t)),
            _ => TrustList::default(),
        }
    }

    pub fn contains(&self, canonical: &str) -> bool {
        self.paths.contains(canonical)
    }

    pub fn save(&self, dirs: &Dirs) -> io::Result<()> {
        let mut text = String::from(
            "# Executables Tabtrail may run with --help to discover their options.\n\
             # Managed by `tabtrail trust` / `tabtrail untrust`; one canonical path per line.\n",
        );
        for p in &self.paths {
            // Never write an entry that could span lines.
            if Self::valid_entry(p) {
                text.push_str(p);
                text.push('\n');
            }
        }
        crate::fsutil::write_atomic(&dirs.trust_file(), text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_overrides() {
        let c = Config::parse("");
        assert_eq!(c, Config::default());
        let c = Config::parse(
            "# comment\nhelp_probes = off\nman_pages=no\nproviders = git\nprobe_timeout_ms = 999999\nbogus = 1\nnot a pair\n",
        );
        assert_eq!(c.help_probes, HelpProbes::Off);
        assert!(!c.man_pages);
        assert_eq!(c.providers.iter().collect::<Vec<_>>(), ["git"]);
        assert_eq!(c.probe_timeout_ms, 30_000);
        assert!(Config::parse("providers = none").providers.is_empty());
    }

    #[test]
    fn trust_list_only_accepts_absolute_paths() {
        let t = TrustList::parse("# c\n/usr/bin/x\nrelative\n  /opt/y  \n/a\rb\n/bidi\u{202e}x\n");
        assert!(t.contains("/usr/bin/x"));
        assert!(t.contains("/opt/y"));
        assert_eq!(t.paths.len(), 2);
        assert!(!TrustList::valid_entry("/x\n/usr/bin/env"));
    }
}
