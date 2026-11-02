//! App review: before a version goes live, appfuzz runs it thousands of
//! times against a simulated platform and reports what it does.
//!
//! The platform writes the appfuzz manifest itself, from the app's
//! `app.toml` and from what only the platform knows: what each call costs,
//! which calls change data, which values it ever returns, and the business
//! rules it enforces. An app can't loosen its own review.
//!
//! The verdict decides: `fail` refuses the version, `warn` lets it go live
//! with its findings shown, `pass` goes live.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;
use crate::platform::PLATFORM_TIME_PER_REQUEST;
use crate::runtime::{FUEL_LIMIT, MEMORY_LIMIT};

/// The oldest appfuzz that understands the manifest written here.
pub const MIN_APPFUZZ: (u32, u32, u32) = (0, 2, 0);

/// How long the shell fuzzes each version before deciding.
pub const DEFAULT_TIME: &str = "10s";

/// The outcome of one review, as stored with the version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    /// "pass", "warn" or "fail".
    pub verdict: String,
    pub findings: Vec<Finding>,
    /// Runs the fuzzer made.
    pub runs: u64,
    /// Where the review's files are kept, for `appfuzz explain`.
    pub dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: String,
    pub kind: String,
    pub title: String,
}

impl Review {
    /// "2 fail, 1 warn: over_budget, serial_chain".
    pub fn summary(&self) -> String {
        let count = |s: &str| self.findings.iter().filter(|f| f.severity == s).count();
        let (fail, warn) = (count("fail"), count("warn"));
        if fail + warn == 0 {
            return format!("no findings in {} runs", self.runs);
        }
        let mut kinds: Vec<&str> = self.findings.iter().map(|f| f.kind.as_str()).collect();
        kinds.dedup();
        let mut counts = Vec::new();
        if fail > 0 {
            counts.push(format!("{fail} fail"));
        }
        if warn > 0 {
            counts.push(format!("{warn} warn"));
        }
        format!("{}: {}", counts.join(", "), kinds.join(", "))
    }
}

/// The appfuzz program: `ROOMER_APPFUZZ`, else `appfuzz` on the PATH.
pub fn appfuzz() -> PathBuf {
    std::env::var_os("ROOMER_APPFUZZ").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("appfuzz"))
}

/// Checks that appfuzz is installed and new enough.
pub fn check_appfuzz() -> Result<String, String> {
    let install = "Install it with: curl -fsSL https://appfuzz.dev/install.sh | sh";
    let out = Command::new(appfuzz()).arg("--version").output().map_err(|_| format!("appfuzz is not installed. {install}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let version = text.split_whitespace().last().unwrap_or("");
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let have = (parts.first().copied().unwrap_or(0), parts.get(1).copied().unwrap_or(0), parts.get(2).copied().unwrap_or(0));
    if have < MIN_APPFUZZ {
        let (a, b, c) = MIN_APPFUZZ;
        return Err(format!("appfuzz {version} is too old; Roomer needs {a}.{b}.{c} or newer. {install}"));
    }
    Ok(version.to_string())
}

/// Reviews one version: writes the wasm and the manifest to `dir`, runs
/// `appfuzz review` there and reads its verdict.
pub fn run(m: &Manifest, wasm: &[u8], dir: &Path, time: &str) -> Result<Review, String> {
    check_appfuzz()?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    std::fs::write(dir.join("app.wasm"), wasm).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("appfuzz.toml"), appfuzz_manifest(m, "app.wasm")).map_err(|e| e.to_string())?;
    let out = Command::new(appfuzz())
        .args(["review", "--time", time])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("could not run appfuzz: {e}"))?;
    let _ = std::fs::write(dir.join("review.json"), &out.stdout);
    parse(&out.stdout, &dir.display().to_string()).map_err(|e| {
        let err = String::from_utf8_lossy(&out.stderr);
        match err.lines().next() {
            Some(line) if !line.is_empty() => format!("{e} ({line})"),
            _ => e,
        }
    })
}

/// Reads the JSON `appfuzz review` printed.
pub fn parse(json: &[u8], dir: &str) -> Result<Review, String> {
    let report: serde_json::Value = serde_json::from_slice(json).map_err(|_| "appfuzz gave no verdict".to_string())?;
    let Some(verdict) = report["verdict"].as_str() else {
        return Err(format!("appfuzz gave no verdict: {}", report["error"].as_str().unwrap_or("unknown error")));
    };
    let findings = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| Finding {
            severity: f["severity"].as_str().unwrap_or("").to_string(),
            kind: f["kind"].as_str().unwrap_or("").to_string(),
            title: f["title"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    Ok(Review {
        verdict: verdict.to_string(),
        findings,
        runs: report["coverage"]["fuzz_runs"].as_u64().unwrap_or(0),
        dir: dir.to_string(),
    })
}

/// A TOML string literal.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The appfuzz manifest for one app: how to call it, what it is granted,
/// its budgets, and what the platform promises about every function.
pub fn appfuzz_manifest(m: &Manifest, wasm: &str) -> String {
    let a = &m.app;
    let (path, slot) = match (m.slots.routes.first(), m.slots.widgets.first()) {
        (Some(route), _) => (route.clone(), "page".to_string()),
        (None, Some(widget)) => {
            let page = crate::manifest::WIDGET_SLOTS.iter().find(|(w, _)| w == widget).map_or("/", |(_, p)| *p);
            (page.to_string(), widget.clone())
        }
        (None, None) => ("/".to_string(), "page".to_string()),
    };
    let today = chrono::Local::now().format("%Y-%m-%d");
    let imports: Vec<String> = m.capabilities.grant.iter().map(|c| q(&format!("appos:platform/{c}"))).collect();
    let mut t = String::new();
    let _ = writeln!(t, "# appfuzz manifest for {} v{}, written by the Roomer platform.", a.id, a.version);
    let _ = writeln!(t, "# Regenerate it with `roomer manifest`; edits are lost.\n");
    let _ = writeln!(t, "[app]\nwasm = {}\nname = {}\n", q(wasm), q(&a.name));
    let _ = writeln!(t, "[entry]");
    let _ = writeln!(t, "export = \"appos:platform/handler#handle\"");
    let _ = writeln!(t, "request = {{ method = \"GET\", path = {}, slot = {}, query = [], form = [], today = \"\", now = \"\" }}", q(&path), q(&slot));
    let _ = writeln!(t, "fields = {{ today = \"clock.date\", now = \"clock.time\" }}");
    let _ = writeln!(t, "values = {{ today = \"format:date\", now = \"format:time\" }}");
    let _ = writeln!(t, "output = {{ status = \"status\", body = \"body\", headers = \"headers\", body_format = \"html\" }}");
    let _ = writeln!(t, "now = \"{today}T10:00:00\"");
    // The shell's CSP blocks scripts; the review also asks that app markup
    // makes no request the user didn't ask for, and never to the shell's own routes.
    let _ = writeln!(t, "result = {{ body = \"html:safe\", \"headers.location\" = \"url:local\" }}");
    let reserved: Vec<String> = crate::manifest::RESERVED_ROUTES.iter().map(|r| q(r)).collect();
    let _ = writeln!(t, "sensitive_paths = [{}]\n", reserved.join(", "));
    let _ = writeln!(t, "[grants]\nimports = [{}]", imports.join(", "));
    let _ = writeln!(t, "wasi = [\"wasi:cli\", \"wasi:clocks\", \"wasi:io\", \"wasi:random\"]\n");
    let _ = writeln!(t, "[limits]");
    let _ = writeln!(t, "fuel = {FUEL_LIMIT}");
    let _ = writeln!(t, "memory = \"{}MiB\"", MEMORY_LIMIT / (1024 * 1024));
    let _ = writeln!(t, "cost_per_request = {}", m.limits.io_per_request);
    let _ = writeln!(t, "max_latency_ms = {}", PLATFORM_TIME_PER_REQUEST.as_millis());
    for cap in &m.capabilities.grant {
        t.push_str(functions(cap));
    }
    t
}

/// What the platform knows about each function of a capability: its cost,
/// its effect, the values it returns, and its rules. Kept next to
/// `platform.rs`, which implements them; `HOST-API.md` describes them.
fn functions(cap: &str) -> &'static str {
    // Literal TOML per capability; the numbers come from platform.rs and
    // are checked against it by the tests below.
    match cap {
        "pods-read" => {
            r#"
[functions."appos:platform/pods-read#all"]
cost = 1
effect = "read"
values = { "" = "len:0..200", name = "len:1..60" }
"#
        }
        "pods-write" => {
            r#"
[functions."appos:platform/pods-write#create"]
cost = 5
effect = "write"
values = { "name" = "len:1..60" }
rules = [
  { when = { name = { regex = "^\\s*$" } }, return = { err = "Name is required" } },
  { when = { name = { regex = "^.{61,}$" } }, return = { err = "Name can be at most 60 characters" } },
]
"#
        }
        "bookings-read" => {
            r#"
[functions."appos:platform/bookings-read#bookable-hours"]
cost = 1
effect = "read"
values = { "" = "range:8..16" }

[functions."appos:platform/bookings-read#list-for-day"]
cost = 1
effect = "read"
values = { date = "format:date", hour = "range:8..16", subject = "len:1..80" }
result = { booker-name = "pii" }
rules = [
  { when = { date = { not = { regex = "^\\d{4}-\\d{2}-\\d{2}$" } } }, return = { err = "not a date" } },
]
"#
        }
        "bookings-write" => {
            r#"
[functions."appos:platform/bookings-write#create"]
cost = 5
effect = "write"
values = { date = "format:date", hour = "range:8..16", subject = "len:1..80" }
result = { booker-name = "pii" }
rules = [
  { when = { hour = { not = { range = [8, 16] } } }, return = { err = "Bookings start between 08:00 and 16:00" } },
  { when = { date = { not = { regex = "^\\d{4}-\\d{2}-\\d{2}$" } } }, return = { err = "not a date" } },
  { when = { subject = { regex = "^\\s*$" } }, return = { err = "Subject is required" } },
  { when = { subject = { regex = "^.{81,}$" } }, return = { err = "Subject can be at most 80 characters" } },
]

[functions."appos:platform/bookings-write#cancel-own"]
cost = 5
effect = "delete"
"#
        }
        "identity" => {
            r#"
[functions."appos:platform/identity#current-user"]
cost = 1
effect = "read"
result = { id = "identity", name = "pii" }
"#
        }
        "log" => {
            r#"
[functions."appos:platform/log#info"]
cost = 1

[functions."appos:platform/log#warn"]
cost = 1
"#
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{BOOKABLE_HOURS, MAX_PODS, READ, WRITE};

    #[test]
    fn numbers_match_the_platform() {
        assert_eq!((READ, WRITE), (1, 5));
        assert_eq!(BOOKABLE_HOURS, 8..=16);
        assert_eq!(MAX_PODS, 200);
    }

    #[test]
    fn every_capability_is_described() {
        for (cap, _) in crate::manifest::CAPABILITIES {
            assert!(!functions(cap).is_empty(), "{cap}");
        }
    }

    #[test]
    fn manifest_is_valid_toml() {
        let m = Manifest::parse(
            r#"
[app]
id = "x"
name = "X \"quoted\""
version = "1.0.0"
author = "a"
[slots]
widgets = ["home.aside"]
[capabilities]
grant = ["pods-read", "pods-write", "bookings-read", "bookings-write", "identity", "log"]
"#,
        )
        .unwrap();
        let text = appfuzz_manifest(&m, "app.wasm");
        let t: toml::Table = text.parse().unwrap();
        assert_eq!(t["entry"]["request"]["slot"].as_str(), Some("home.aside"));
        assert_eq!(t["limits"]["cost_per_request"].as_integer(), Some(100));
    }
}
