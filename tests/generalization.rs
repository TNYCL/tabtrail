//! Generalization check on help texts the parser was not developed against.
//!
//! The fixtures in tests/fixtures/help/holdout were written, together with
//! the expectations below, before the parser was run on them. Expectations
//! describe what a careful reader of each help text would extract. The test
//! reports every individual check so partial results stay visible.

use tabtrail::help::parse_help;
use tabtrail::schema::{CommandNode, ValueKind};

fn load(name: &str) -> CommandNode {
    load_from("holdout", name)
}

fn load_from(dir: &str, name: &str) -> CommandNode {
    let path = format!("{}/tests/fixtures/help/{dir}/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
    parse_help(&text, &[name_of(name)])
}

fn name_of(fixture: &str) -> &'static str {
    match fixture {
        "clap2_caps" => "archiver",
        "docopt" => "navalfate",
        "yargs" => "bundler",
        "picocli" => "jtool",
        "busybox" => "bbtool",
        "prose_trap" => "traptool",
        "typer" => "shipit",
        "swift" => "swifty",
        "oclif" => "cloudy",
        "clikt" => "kli",
        "abseil" => "absl_tool",
        "dotnet" => "netcli",
        _ => unreachable!(),
    }
}

enum Expect {
    Flags(&'static [&'static str]),
    TakesValue(&'static str),
    NoValue(&'static str),
    Choices(&'static str, &'static [&'static str]),
    Subcommands(&'static [&'static str]),
    NoFlags(&'static [&'static str]),
    NoSubcommands,
}

fn check(node: &CommandNode, e: &Expect) -> Result<(), String> {
    let names: Vec<String> = node.flags.iter().flat_map(|f| f.names.clone()).collect();
    let subs: Vec<&str> = node.subcommands.iter().map(|s| s.name.as_str()).collect();
    match e {
        Expect::Flags(want) => {
            let missing: Vec<_> = want.iter().filter(|w| !names.iter().any(|n| n == *w)).collect();
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!("missing flags {missing:?}; have {names:?}"))
            }
        }
        Expect::TakesValue(f) => match node.flag(f) {
            Some(fl) if fl.takes_separate_value() => Ok(()),
            Some(_) => Err(format!("{f} should take a value")),
            None => Err(format!("{f} missing")),
        },
        Expect::NoValue(f) => match node.flag(f) {
            Some(fl) if fl.value.is_none() => Ok(()),
            Some(_) => Err(format!("{f} should not take a value")),
            None => Err(format!("{f} missing")),
        },
        Expect::Choices(f, want) => {
            let got: Vec<&str> = node
                .flag(f)
                .and_then(|fl| fl.value.as_ref())
                .filter(|v| v.kind == ValueKind::Choice)
                .map(|v| v.choices.iter().map(|c| c.value.as_str()).collect())
                .unwrap_or_default();
            if got == *want {
                Ok(())
            } else {
                Err(format!("{f} choices {got:?}, want {want:?}"))
            }
        }
        Expect::Subcommands(want) => {
            if subs == *want {
                Ok(())
            } else {
                Err(format!("subcommands {subs:?}, want {want:?}"))
            }
        }
        Expect::NoFlags(bad) => {
            let present: Vec<_> = bad.iter().filter(|b| names.iter().any(|n| n == *b)).collect();
            if present.is_empty() {
                Ok(())
            } else {
                Err(format!("invented flags {present:?}"))
            }
        }
        Expect::NoSubcommands => {
            if subs.is_empty() {
                Ok(())
            } else {
                Err(format!("invented subcommands {subs:?}"))
            }
        }
    }
}

fn evaluate(dir: &str, cases: Vec<(&str, Vec<Expect>)>) -> (usize, Vec<String>) {
    let mut total = 0;
    let mut failures = Vec::new();
    for (fixture, expects) in &cases {
        let node = load_from(dir, fixture);
        for e in expects {
            total += 1;
            if let Err(msg) = check(&node, e) {
                failures.push(format!("{fixture}: {msg}"));
            }
        }
    }
    println!("{dir}: {}/{} checks passed", total - failures.len(), total);
    for f in &failures {
        println!("  FAIL {f}");
    }
    (total, failures)
}

/// Second holdout set, written after the v1 fixes and before running the
/// parser on it. Known misses are listed in KNOWN_GAPS_V2 (reported, not
/// hidden); any other failure, or a known gap that starts passing, fails.
#[test]
fn holdout_v2_fixtures() {
    use Expect::*;
    let cases: Vec<(&str, Vec<Expect>)> = vec![
        (
            "typer",
            vec![
                Flags(&["--region", "--verbose", "-v", "--help"]),
                Choices("--region", &["eu", "us", "ap"]),
                NoValue("--verbose"),
                Subcommands(&["launch", "recall"]),
            ],
        ),
        (
            "swift",
            vec![
                Flags(&["--log-level", "-h", "--help"]),
                TakesValue("--log-level"),
                Choices("--log-level", &["trace", "debug", "info"]),
                Subcommands(&["compile", "test"]),
            ],
        ),
        ("oclif", vec![Subcommands(&["apps", "login", "help"])]),
        (
            "clikt",
            vec![
                Flags(&["--color", "-n", "--dry-run", "-h", "--help"]),
                Choices("--color", &["auto", "always", "never"]),
                NoValue("--dry-run"),
                Subcommands(&["sync", "purge"]),
            ],
        ),
        (
            "abseil",
            vec![
                Flags(&["--input", "--max_items", "--fast", "--nofast"]),
                TakesValue("--input"),
                NoValue("--fast"),
            ],
        ),
        (
            "dotnet",
            vec![
                Flags(&["-o", "--output", "--format", "-?", "-h", "--help"]),
                TakesValue("--output"),
                Choices("--format", &["json", "text", "xml"]),
                Subcommands(&["new", "run"]),
            ],
        ),
    ];
    let (_, failures) = evaluate("holdout_v2", cases);
    let known: Vec<String> = KNOWN_GAPS_V2.iter().map(|s| s.to_string()).collect();
    let unexpected: Vec<&String> = failures.iter().filter(|f| !known.contains(f)).collect();
    let fixed: Vec<&String> = known.iter().filter(|k| !failures.contains(k)).collect();
    assert!(unexpected.is_empty(), "unexpected holdout failures: {unexpected:#?}");
    assert!(
        fixed.is_empty(),
        "known gaps now pass; update KNOWN_GAPS_V2: {fixed:#?}"
    );
}

/// Filled in from the first (untuned) run of holdout v2.
const KNOWN_GAPS_V2: &[&str] = &[];

#[test]
fn holdout_fixtures() {
    use Expect::*;
    let cases: Vec<(&str, Vec<Expect>)> = vec![
        (
            "clap2_caps",
            vec![
                Flags(&[
                    "-h",
                    "--help",
                    "-q",
                    "--quiet",
                    "-V",
                    "--version",
                    "-l",
                    "--level",
                    "-o",
                    "--output-dir",
                ]),
                NoValue("--quiet"),
                TakesValue("--output-dir"),
                Choices("--level", &["fast", "normal", "best"]),
                Subcommands(&["pack", "unpack", "list", "help"]),
            ],
        ),
        (
            "docopt",
            vec![
                Flags(&["-h", "--help", "--version", "--speed", "--moored"]),
                NoValue("--moored"),
                Subcommands(&["ship"]),
            ],
        ),
        (
            "yargs",
            vec![
                Flags(&["--version", "-m", "--mode", "-c", "--config", "--help"]),
                TakesValue("--mode"),
                Choices("--mode", &["development", "production"]),
                NoValue("--help"),
                Subcommands(&["build", "watch", "serve"]),
            ],
        ),
        (
            "picocli",
            vec![
                Flags(&[
                    "-f",
                    "--format",
                    "-h",
                    "--help",
                    "--threads",
                    "-v",
                    "--verbose",
                    "-V",
                    "--version",
                ]),
                Choices("--format", &["XML", "JSON", "CSV"]),
                NoValue("--verbose"),
                Subcommands(&["import", "export"]),
            ],
        ),
        ("busybox", vec![Flags(&["-r", "-t"]), NoValue("-r"), TakesValue("-t")]),
        (
            "prose_trap",
            vec![NoFlags(&["--frobnicate", "-x", "--not-a-flag"]), NoSubcommands],
        ),
    ];
    let mut total = 0;
    let mut failures = Vec::new();
    for (fixture, expects) in &cases {
        let node = load(fixture);
        for e in expects {
            total += 1;
            if let Err(msg) = check(&node, e) {
                failures.push(format!("{fixture}: {msg}"));
            }
        }
    }
    println!("holdout: {}/{} checks passed", total - failures.len(), total);
    for f in &failures {
        println!("  FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} holdout checks failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
