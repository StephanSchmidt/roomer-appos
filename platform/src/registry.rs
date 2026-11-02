//! The app registry: deploys, review, activation and rollback.
//!
//! A new version passes two gates before it goes live:
//! - **The sandbox**: it is refused if its code uses a capability its
//!   manifest does not declare. The manifest is the grant.
//! - **App review**: appfuzz runs it thousands of times against a simulated
//!   platform. A `fail` finding refuses it, a `warn` lets it go live with
//!   the finding shown, and a pass goes live.
//!
//! A refused version never replaces the live one.

use std::path::Path;
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{self, now};
use crate::manifest::{Manifest, routes_overlap};
use crate::platform::Platform;
use crate::review::Review;
use crate::runtime;

#[derive(Debug, Serialize)]
pub struct Outcome {
    /// "live", "flagged" (live, with review findings) or "refused"
    pub status: &'static str,
    pub message: String,
}

impl Outcome {
    fn refused(message: String) -> Self {
        Outcome { status: "refused", message }
    }
}

fn active_manifest(p: &Platform, app_id: &str) -> Option<(String, Manifest)> {
    let db = p.db();
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT version, manifest FROM app_versions WHERE app_id = ?1 AND status = 'active'",
            [app_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    row.and_then(|(v, m)| Manifest::parse(&m).ok().map(|m| (v, m)))
}

fn sha256(wasm: &[u8]) -> String {
    Sha256::digest(wasm).iter().map(|b| format!("{b:02x}")).collect()
}

/// Reviews a version with appfuzz. Its files stay in the data directory, so
/// `appfuzz explain` works there afterwards.
fn review(p: &Platform, manifest: &Manifest, wasm: &[u8], hash: &str) -> Result<Review, String> {
    let dir = Path::new(crate::DATA_DIR).join("reviews").join(format!("{}-{}-{}", manifest.app.id, manifest.app.version, &hash[..12]));
    crate::review::run(manifest, wasm, &dir, &p.review_time)
}

/// What a review means for the deploy message.
fn review_text(r: &Review) -> String {
    match r.verdict.as_str() {
        "pass" => format!("App review passed ({})", r.summary()),
        "warn" => format!("App review warns: {}", r.summary()),
        _ => format!("App review failed: {}", r.summary()),
    }
}

pub fn deploy(p: &Platform, manifest_text: &str, wasm: &[u8], deployed_by: &str) -> Outcome {
    let manifest = match Manifest::parse(manifest_text) {
        Ok(m) => m,
        Err(e) => return Outcome::refused(e),
    };
    let app_id = manifest.app.id.clone();
    let version = manifest.app.version.clone();
    let hash = sha256(wasm);

    // The same version again: only allowed for the very same code, to review
    // a version that was installed before review.
    let existing: Option<(i64, String, Option<String>)> = p
        .db()
        .query_row(
            "SELECT id, wasm_hash, review FROM app_versions
             WHERE app_id = ?1 AND version = ?2 AND status IN ('pending', 'active', 'retired')",
            params![app_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    if let Some((row_id, old_hash, old_review)) = existing {
        if old_hash != hash || old_review.is_some() {
            return Outcome::refused(format!("{app_id} {version} is already deployed; bump the version"));
        }
        return review_installed(p, &manifest, wasm, &hash, row_id);
    }

    let active = active_manifest(p, &app_id);
    let keeps_serving = match &active {
        Some((v, _)) => format!("{app_id} {v} keeps serving."),
        None => "Nothing changed for users.".to_string(),
    };

    let (imports, check) = match runtime::compile(&p.engine, wasm) {
        Ok((_, imports)) => {
            let check = runtime::check_imports(&imports, &manifest);
            (imports, check)
        }
        Err(e) => (vec![], Err(format!("{e:#}"))),
    };

    let record = |status: &str, reason: Option<&str>, review: Option<&Review>| {
        let db = p.db();
        db.execute(
            "INSERT INTO app_versions (app_id, version, name, author, manifest, wasm, wasm_hash, imports, status, reason, review, deployed_by, deployed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                app_id,
                version,
                manifest.app.name,
                manifest.app.author,
                manifest_text,
                if status == "refused" { None } else { Some(wasm) },
                hash,
                serde_json::to_string(&imports).unwrap_or_default(),
                status,
                reason,
                review.and_then(|r| serde_json::to_string(r).ok()),
                deployed_by,
                now(),
            ],
        )
    };

    if let Err(reason) = check {
        let _ = record("refused", Some(&reason), None);
        let message = format!("Refused by the sandbox: the app {reason}. {keeps_serving}");
        db::event(&p.db(), &app_id, &version, "refused", &message, "roomer");
        return Outcome::refused(message);
    }

    let review = match review(p, &manifest, wasm, &hash) {
        Ok(r) => r,
        Err(e) => {
            let reason = format!("could not review it: {e}");
            let _ = record("refused", Some(&reason), None);
            let message = format!("Refused: {reason} {keeps_serving}");
            db::event(&p.db(), &app_id, &version, "refused", &message, "appfuzz");
            return Outcome::refused(message);
        }
    };
    if review.verdict == "fail" {
        let reason = review_text(&review);
        let _ = record("refused", Some(&reason), Some(&review));
        let message = format!("Refused in review: {}. {keeps_serving}", review.summary());
        db::event(&p.db(), &app_id, &version, "refused", &message, "appfuzz");
        return Outcome::refused(message);
    }

    // Stored as pending only until it is loaded, a moment later.
    if let Err(e) = record("pending", None, Some(&review)) {
        return Outcome::refused(format!("could not store version: {e}"));
    }
    match activate(p, &app_id, &version, deployed_by, "deployed") {
        Ok(()) => {
            let live = format!("Live with {}.", granted(&manifest));
            if review.verdict == "warn" {
                db::event(&p.db(), &app_id, &version, "flagged", &review_text(&review), "appfuzz");
                Outcome { status: "flagged", message: format!("{live} {}.", review_text(&review)) }
            } else {
                Outcome { status: "live", message: format!("{live} {}.", review_text(&review)) }
            }
        }
        Err(e) => {
            let _ = p.db().execute(
                "UPDATE app_versions SET status = 'refused', reason = ?3, wasm = NULL
                 WHERE app_id = ?1 AND version = ?2 AND status = 'pending'",
                params![app_id, version, e],
            );
            let message = format!("Refused: {e}. {keeps_serving}");
            db::event(&p.db(), &app_id, &version, "refused", &message, "roomer");
            Outcome::refused(message)
        }
    }
}

/// Reviews a version that was installed before review, without changing
/// what is live: a failing app that is already serving keeps serving until
/// a fixed version replaces it.
fn review_installed(p: &Platform, manifest: &Manifest, wasm: &[u8], hash: &str, row_id: i64) -> Outcome {
    let (app_id, version) = (&manifest.app.id, &manifest.app.version);
    let review = match review(p, manifest, wasm, hash) {
        Ok(r) => r,
        Err(e) => return Outcome::refused(format!("Could not review {app_id} {version}: {e}")),
    };
    let _ = p.db().execute("UPDATE app_versions SET review = ?2 WHERE id = ?1", params![row_id, serde_json::to_string(&review).ok()]);
    let text = review_text(&review);
    match review.verdict.as_str() {
        "fail" => {
            let message = format!(
                "{text}. v{version} was installed before review and keeps serving; fix it, bump the version and deploy again."
            );
            db::event(&p.db(), app_id, version, "review-failed", &message, "appfuzz");
            Outcome::refused(message)
        }
        "warn" => {
            db::event(&p.db(), app_id, version, "flagged", &format!("{text} (installed before review)"), "appfuzz");
            Outcome { status: "flagged", message: format!("Already live. {text}.") }
        }
        _ => {
            db::event(&p.db(), app_id, version, "reviewed", &format!("{text} (installed before review)"), "appfuzz");
            Outcome { status: "live", message: format!("Already live. {text}.") }
        }
    }
}

/// Installs the apps built into the binary, the first time the shell runs.
/// They are live at once, and marked as installed before review.
pub fn install_preinstalled(p: &Platform) {
    let empty: bool = p.db().query_row("SELECT NOT EXISTS(SELECT 1 FROM app_versions)", [], |r| r.get(0)).unwrap_or(false);
    if !empty {
        return;
    }
    for (manifest_text, wasm) in crate::embedded::APPS {
        let Ok(manifest) = Manifest::parse(manifest_text) else { continue };
        let imports = runtime::compile(&p.engine, wasm).map(|(_, i)| i).unwrap_or_default();
        let (app_id, version) = (&manifest.app.id, &manifest.app.version);
        let db = p.db();
        let inserted = db.execute(
            "INSERT INTO app_versions (app_id, version, name, author, manifest, wasm, wasm_hash, imports, status, reason, review, deployed_by, deployed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', NULL, NULL, 'roomer', ?9)",
            params![app_id, version, manifest.app.name, manifest.app.author, manifest_text, wasm, sha256(wasm), serde_json::to_string(&imports).unwrap_or_default(), now()],
        );
        if inserted.is_ok() {
            db::event(&db, app_id, version, "preinstalled", "Installed with Roomer, before app review existed", "roomer");
        }
    }
}

/// "pods-read and bookings-read", for the deploy message.
fn granted(m: &Manifest) -> String {
    match m.capabilities.grant.as_slice() {
        [] => "no capabilities".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Makes a version the live one: a fresh deploy, or a rollback to an earlier
/// version.
pub fn activate(p: &Platform, app_id: &str, version: &str, actor: &str, kind: &str) -> Result<(), String> {
    let (row_id, manifest_text, wasm): (i64, String, Option<Vec<u8>>) = p
        .db()
        .query_row(
            "SELECT id, manifest, wasm FROM app_versions
             WHERE app_id = ?1 AND version = ?2 AND status IN ('pending', 'retired')",
            params![app_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| format!("{app_id} {version} is not available to make live"))?;
    let wasm = wasm.ok_or("no code stored for this version")?;
    let manifest = Manifest::parse(&manifest_text)?;

    // Routes belong to exactly one app.
    for (other_id, other) in p.apps.read().unwrap_or_else(|e| e.into_inner()).iter() {
        if other_id != app_id
            && let Some(route) = routes_overlap(&manifest.slots.routes, &other.manifest.slots.routes)
        {
            return Err(format!("route {route} already belongs to {other_id}"));
        }
    }

    let grants = manifest.capabilities.grant.clone();
    let (component, _) = runtime::compile(&p.engine, &wasm).map_err(|e| format!("{e:#}"))?;
    let loaded = runtime::load(&p.engine, &component, manifest).map_err(|e| format!("could not load: {e:#}"))?;
    p.policy.set_app_grants(app_id, &grants).map_err(|e| format!("could not update policy: {e:#}"))?;

    {
        let db = p.db();
        db.execute(
            "UPDATE app_versions SET status = 'retired' WHERE app_id = ?1 AND status = 'active'",
            [app_id],
        )
        .map_err(|e| e.to_string())?;
        db.execute("UPDATE app_versions SET status = 'active' WHERE id = ?1", [row_id])
        .map_err(|e| e.to_string())?;
        let message = match kind {
            "deployed" => format!("Deployed by {actor}, now live"),
            "rolled-back" => format!("Rolled back to this version by {actor}"),
            "auto-rolled-back" => "Rolled back to this version automatically".to_string(),
            _ => "Live".to_string(),
        };
        db::event(&db, app_id, version, kind, &message, actor);
    }
    p.apps.write().unwrap_or_else(|e| e.into_inner()).insert(app_id.to_string(), Arc::new(loaded));
    p.refresh_nav();
    Ok(())
}

/// Failed requests within this window count towards an automatic rollback.
const FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const FAILURES_BEFORE_ROLLBACK: usize = 5;

/// Records a failed request. If the live version keeps failing, it is rolled
/// back to the version that was live before it.
pub fn note_failure(p: &Platform, app_id: &str, version: &str) {
    let now = std::time::Instant::now();
    let failing = {
        let mut failures = p.failures.lock().unwrap_or_else(|e| e.into_inner());
        let recent = failures.entry(app_id.to_string()).or_default();
        recent.retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
        recent.push(now);
        recent.len() >= FAILURES_BEFORE_ROLLBACK
    };
    if !failing {
        return;
    }
    let previous: Option<String> = p
        .db()
        .query_row(
            "SELECT version FROM app_versions
             WHERE app_id = ?1 AND status = 'retired'
               AND id < (SELECT id FROM app_versions WHERE app_id = ?1 AND version = ?2 AND status = 'active')
             ORDER BY id DESC LIMIT 1",
            params![app_id, version],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(previous) = previous else { return };
    if activate(p, app_id, &previous, "AppOS", "auto-rolled-back").is_ok() {
        p.failures.lock().unwrap_or_else(|e| e.into_inner()).remove(app_id);
        let reason = format!("{FAILURES_BEFORE_ROLLBACK} failed requests within a minute");
        let db = p.db();
        let _ = db.execute(
            "UPDATE app_versions SET status = 'failed', reason = ?3 WHERE app_id = ?1 AND version = ?2 AND status = 'retired'",
            params![app_id, version, reason],
        );
        db::event(
            &db,
            app_id,
            version,
            "failed",
            &format!("v{version} had {reason}; AppOS rolled back to v{previous} automatically"),
            "roomer",
        );
    }
}

/// Loads every active version at startup.
pub fn load_active(p: &Platform) {
    let rows: Vec<(String, String, String, Vec<u8>)> = {
        let db = p.db();
        let Ok(mut stmt) =
            db.prepare("SELECT app_id, version, manifest, wasm FROM app_versions WHERE status = 'active' AND wasm IS NOT NULL")
        else {
            return;
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    };
    for (app_id, version, manifest_text, wasm) in rows {
        let loaded = Manifest::parse(&manifest_text).map_err(anyhow::Error::msg).and_then(|m| {
            let (component, _) = runtime::compile(&p.engine, &wasm)?;
            runtime::load(&p.engine, &component, m)
        });
        match loaded {
            Ok(app) => {
                if let Err(e) = p.policy.set_app_grants(&app_id, &app.manifest.capabilities.grant) {
                    eprintln!("could not set policy for {app_id}: {e:#}");
                    continue;
                }
                println!("loaded {app_id} {version}");
                p.apps.write().unwrap_or_else(|e| e.into_inner()).insert(app_id, Arc::new(app));
                p.refresh_nav();
            }
            Err(e) => eprintln!("could not load {app_id} {version}: {e:#}"),
        }
    }
}
