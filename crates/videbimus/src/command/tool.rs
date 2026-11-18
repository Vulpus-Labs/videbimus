//! Tools' version probes, run once per process and cached.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use regex::Regex;

/// What a probe printed, or why it couldn't run.
#[derive(Clone, Debug)]
struct Probed {
    ok: bool,
    status: String,
    stdout: String,
    stderr: String,
}

type Cache = Mutex<HashMap<Vec<String>, Result<Probed, String>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Cache::default)
}

/// Forgets every tool version probed so far, so the next load probes
/// again. A build probes each tool once per process; a long-lived
/// process (a watcher, a test) that wants to see a changed tool calls
/// this.
pub fn forget_tool_versions() {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn probe(argv: &[String]) -> Result<Probed, String> {
    let mut guard = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(p) = guard.get(argv) {
        return p.clone();
    }
    let result = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map(|o| Probed {
            ok: o.status.success(),
            status: o.status.to_string(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
        .map_err(|e| e.to_string());
    guard.insert(argv.to_vec(), result.clone());
    result
}

/// A failed probe: what happened, notes, and the fix.
#[derive(Debug)]
pub(crate) struct ProbeError {
    pub message: String,
    pub notes: Vec<String>,
    pub help: String,
}

fn last_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| format!("| {l}"))
        .collect()
}

/// Tool `name`'s version: runs `argv` (cached) and reads its first
/// non-empty line, from stdout or else stderr, or with `pattern` the
/// first match (its first group, if it has one) in either.
pub(crate) fn version(
    name: &str,
    shown: &str,
    argv: &[String],
    pattern: Option<&Regex>,
    has_path: bool,
) -> Result<String, ProbeError> {
    let fail = |message: String, notes: Vec<String>, help: String| ProbeError {
        message,
        notes,
        help,
    };
    let p = probe(argv).map_err(|e| {
        let help = if has_path {
            format!("check tool \"{name}\"'s `path`, and that it runs")
        } else {
            format!(
                "install `{name}`, or give its path: tool \"{name}\" path=\"/path/to/{name}\" version-from=\"{shown}\""
            )
        };
        fail(
            format!("tool `{name}` wasn't found: its version probe `{shown}` couldn't run ({e})"),
            Vec::new(),
            help,
        )
    })?;
    let output = format!("{}{}", p.stdout, p.stderr);
    if !p.ok {
        return Err(fail(
            format!(
                "tool `{name}`: its version probe `{shown}` failed ({})",
                p.status
            ),
            last_lines(&output, 5),
            "run the probe by hand; `version-from` is a command that prints the version".to_owned(),
        ));
    }
    let found = match pattern {
        Some(re) => re
            .captures(&p.stdout)
            .or_else(|| re.captures(&p.stderr))
            .map(|c| {
                c.get(1)
                    .or_else(|| c.get(0))
                    .map_or(String::new(), |m| m.as_str().trim().to_owned())
            }),
        None => p
            .stdout
            .lines()
            .chain(p.stderr.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned),
    };
    match found {
        Some(v) if !v.is_empty() => Ok(v),
        _ => {
            let what = match pattern {
                Some(re) => format!("nothing matching `{}`", re.as_str()),
                None => "nothing".to_owned(),
            };
            Err(fail(
                format!("tool `{name}`: its version probe `{shown}` printed {what}"),
                last_lines(&output, 5),
                "fix `version-from`, or `version-match` (a regex; its first group is the version)"
                    .to_owned(),
            ))
        }
    }
}
