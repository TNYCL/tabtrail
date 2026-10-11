//! The foreground completion path: request → context → resolution → cached
//! schema → engine (+ provider) → shell rendering. It never waits for
//! discovery and degrades to "no candidates, shell fallback" on any problem.

use std::path::PathBuf;

use crate::cache::{Cache, Lookup};
use crate::completion::{Candidate, CandidateKind, Outcome, complete as engine};
use crate::config::{Config, Dirs, TrustList};
use crate::context::{CommandContext, Context, CurrentWord, parse};
use crate::discovery::{Policy, spawn_background};
use crate::providers::{self, ProviderId};
use crate::render;
use crate::resolve::{Resolved, resolve};
use crate::schema::NodeStatus;
use crate::shellword::Dialect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Shell> {
        match s {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }

    pub fn dialect(self) -> Dialect {
        match self {
            Shell::Bash => Dialect::Bash,
            Shell::Zsh => Dialect::Zsh,
            Shell::Fish => Dialect::Fish,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub shell: Shell,
    /// Command line text before the cursor.
    pub before: String,
    /// Bash COMP_WORDBREAKS.
    pub wordbreaks: Option<String>,
    /// What the shell reports for the first word: alias, function, builtin,
    /// keyword, file, or empty when unknown.
    pub command_kind: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Debug)]
pub struct Evaluation {
    pub context: Context,
    pub note: &'static str,
    pub resolved: Option<Resolved>,
    pub cache: &'static str,
    pub policy: Option<Policy>,
    pub outcome: Outcome,
    pub provider: Option<(ProviderId, providers::Permission)>,
    pub discovery_requested: Option<Vec<String>>,
}

fn empty_current(before: &str) -> CurrentWord {
    CurrentWord {
        value: String::new(),
        start: before.len(),
        offsets: Vec::new(),
        open_quote: None,
        expansion: false,
        subst: false,
        quoted: false,
    }
}

fn fallback(context: Context, note: &'static str) -> Evaluation {
    Evaluation {
        context,
        note,
        resolved: None,
        cache: "-",
        policy: None,
        outcome: Outcome {
            files: true,
            ..Default::default()
        },
        provider: None,
        discovery_requested: None,
    }
}

/// Evaluates a request. `spawn` controls whether background discovery may
/// be started (false for diagnostics).
pub fn evaluate(req: &Request, dirs: Option<&Dirs>, spawn: bool) -> Evaluation {
    let context = parse(&req.before, req.shell.dialect());
    let Context::Command(ctx) = &context else {
        return fallback(context, "not an argument position Tabtrail handles");
    };
    let ctx: CommandContext = ctx.clone();
    if ctx.command_is_first_word
        && matches!(
            req.command_kind.as_deref(),
            Some("alias" | "function" | "builtin" | "keyword")
        )
    {
        return fallback(context, "command word is an alias, function, builtin or keyword");
    }
    let path_var = std::env::var_os("PATH");
    let Some(resolved) = resolve(&ctx.command, path_var.as_deref(), &req.cwd) else {
        return fallback(context, "command is not an executable on PATH");
    };
    let Some(dirs) = dirs else {
        return fallback(context, "no HOME/XDG directories");
    };
    let config = Config::load(dirs);
    let trust = TrustList::load(dirs);
    let policy = Policy::decide(&config, &trust, &resolved.identity.canonical).with_man_roots(
        &resolved.identity.path,
        std::env::var_os("MANPATH").as_deref(),
        path_var.as_deref(),
    );
    let cache = Cache::new(dirs);
    let lookup = cache.load(&resolved.identity, policy.man_roots);
    let (doc, cache_state) = match lookup {
        Lookup::Fresh(doc) => (Some(doc), "fresh"),
        Lookup::Stale => (None, "stale"),
        Lookup::Missing => (None, "missing"),
        Lookup::Corrupt => (None, "corrupt"),
    };
    // A document whose root yielded nothing is "known to be unknown": use
    // no schema, and only rediscover when the policy has widened since.
    let root = doc
        .as_ref()
        .map(|d| &d.root)
        .filter(|r| r.status == NodeStatus::Described);
    let mut outcome = engine(root, &ctx);
    match &doc {
        Some(d) if policy.wider_than(d.collected_with) => {
            outcome.discover.get_or_insert_with(Vec::new);
        }
        Some(_) if root.is_none() => outcome.discover = None,
        _ => {}
    }

    let mut provider = None;
    if let Some((path, pos)) =
        providers::effective_slot(outcome.slot.as_ref(), root.is_some(), &ctx.args, &ctx.current.value)
        && !ctx.current.expansion
        && let Some(id) = providers::select(&resolved.name, &path, pos)
    {
        // Runs only the user's own resolved executable, and only if
        // it is trusted, the provider is enabled, and the command targets
        // the same repository/daemon/user context the provider would query.
        let env_lookup = |k: &str| std::env::var_os(k);
        let mut permission = providers::permission(id, &config, &trust, &resolved);
        if permission == providers::Permission::Allowed
            && let Err(reason) = providers::context(id, &ctx.args, ctx.command_is_first_word, &env_lookup)
        {
            permission = providers::Permission::ContextUnsupported(reason);
        }
        provider = Some((id, permission));
        let existing: Vec<String> = outcome.candidates.iter().map(|c| c.value.clone()).collect();
        let found = if permission == providers::Permission::Allowed {
            providers::values(
                id,
                &config,
                &trust,
                &resolved,
                &ctx.args,
                ctx.command_is_first_word,
                &env_lookup,
                &req.cwd,
                &ctx.current.value,
            )
        } else {
            Vec::new()
        };
        for v in found {
            if !existing.contains(&v) {
                outcome.candidates.push(Candidate {
                    value: v,
                    description: None,
                    kind: CandidateKind::Value,
                    no_space: false,
                });
            }
        }
    }

    let discovery_requested = outcome.discover.clone();
    if spawn && let Some(sub) = &discovery_requested {
        spawn_background(
            &cache,
            &resolved,
            sub,
            policy,
            matches!(cache_state, "stale" | "corrupt"),
        );
    }
    Evaluation {
        context,
        note: "ok",
        resolved: Some(resolved),
        cache: cache_state,
        policy: Some(policy),
        outcome,
        provider,
        discovery_requested,
    }
}

pub fn render(req: &Request, ev: &Evaluation) -> Vec<u8> {
    let empty = empty_current(&req.before);
    let cur = match &ev.context {
        Context::Command(c) => &c.current,
        _ => &empty,
    };
    match req.shell {
        Shell::Bash => render::render_bash(
            &req.before,
            cur,
            &ev.outcome,
            req.wordbreaks.as_deref().unwrap_or(" \t\n\"'><=;|&(:"),
        ),
        Shell::Zsh => render::render_zsh(&req.before, cur, &ev.outcome),
        Shell::Fish => render::render_fish(cur, &ev.outcome),
    }
}

pub fn complete(req: &Request) -> Vec<u8> {
    let dirs = Dirs::from_env();
    let ev = evaluate(req, dirs.as_ref(), true);
    render(req, &ev)
}

/// Human-readable explanation of how a line would be completed.
pub fn explain(req: &Request) -> String {
    let dirs = Dirs::from_env();
    let ev = evaluate(req, dirs.as_ref(), false);
    let mut s = String::new();
    let push = |s: &mut String, k: &str, v: String| s.push_str(&format!("{k:<12} {v}\n"));
    push(&mut s, "shell", req.shell.name().into());
    match &ev.context {
        Context::Command(c) => {
            push(&mut s, "command", format!("{:?}", c.command));
            push(
                &mut s,
                "arguments",
                format!("{:?}", c.args.iter().map(|a| &a.value).collect::<Vec<_>>()),
            );
            push(&mut s, "current", format!("{:?}", c.current.value));
        }
        other => push(&mut s, "context", format!("{other:?}")),
    }
    push(&mut s, "status", ev.note.into());
    if let Some(r) = &ev.resolved {
        push(
            &mut s,
            "executable",
            format!("{} -> {}", r.identity.path, r.identity.canonical),
        );
        push(&mut s, "cache", ev.cache.into());
    }
    if let Some(p) = ev.policy {
        let help = if p.help {
            "allowed (trusted)"
        } else {
            "not allowed (run `tabtrail trust <command>` to allow)"
        };
        push(&mut s, "help probes", help.into());
        push(&mut s, "man pages", if p.man { "allowed" } else { "disabled" }.into());
    }
    if let Some(sub) = &ev.discovery_requested {
        push(&mut s, "discovery", format!("needed for path {sub:?}"));
    }
    if let Some((p, perm)) = ev.provider {
        let state = match perm {
            providers::Permission::Allowed => "runs (trusted)".to_string(),
            providers::Permission::Disabled => "disabled in configuration".to_string(),
            providers::Permission::Untrusted => format!("not run: `tabtrail trust {}` to allow", p.name()),
            providers::Permission::ContextUnsupported(why) => format!("not run: {why}"),
        };
        push(&mut s, "provider", format!("{} - {state}", p.name()));
    }
    push(&mut s, "files", ev.outcome.files.to_string());
    push(&mut s, "candidates", ev.outcome.candidates.len().to_string());
    for c in &ev.outcome.candidates {
        let d = c.description.as_deref().unwrap_or("");
        s.push_str(&format!(
            "  {:<10} {}  {}\n",
            c.kind.as_str(),
            crate::sanitize::display_text(&c.value, 80),
            d
        ));
    }
    s
}
