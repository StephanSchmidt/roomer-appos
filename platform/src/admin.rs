//! The "Apps in Roomer" admin screen, ordered by what the platform team does:
//! notice what went wrong, see what is live and roll it back. Nothing waits
//! for a person's approval: the manifest is the grant, and app review
//! (appfuzz) decides whether a version may go live. History, the full log
//! and the access policy are one click away.

use chrono::NaiveDateTime;
use maud::{Markup, html};
use rusqlite::{Connection, OptionalExtension};

use crate::db::User;
use crate::manifest::{CAPABILITIES, Manifest, describe, risk};
use crate::policy::Policy;
use crate::views;

struct Version {
    id: i64,
    app_id: String,
    version: String,
    name: String,
    author: String,
    manifest: Option<Manifest>,
    imports: Vec<String>,
    status: String,
    reason: Option<String>,
    /// None: installed before review.
    review: Option<crate::review::Review>,
    deployed_by: String,
    deployed_at: String,
}

struct Event {
    id: i64,
    at: String,
    app_id: String,
    version: String,
    kind: String,
    message: String,
    actor: String,
}

/// Event kinds that mean something went wrong.
const PROBLEMS: &[&str] = &["refused", "review-failed", "flagged", "denied", "over-budget", "crashed", "failed"];

/// Event kinds about deploys, as opposed to what happened while serving.
const DEPLOYS: &[&str] = &["deployed", "preinstalled", "reviewed", "review-failed", "flagged", "rolled-back", "auto-rolled-back", "refused", "failed",
    // Kinds from when versions waited for approval, still in older logs.
    "needs-approval", "auto-approved", "approved", "rejected"];

/// Events shown on the overview; the activity view shows more.
const RECENT_EVENTS: usize = 8;
const ALL_EVENTS: usize = 200;

fn versions(db: &Connection) -> Vec<Version> {
    let Ok(mut stmt) = db.prepare(
        "SELECT app_id, version, name, author, manifest, imports, status, reason, deployed_by, deployed_at, id, review
         FROM app_versions ORDER BY id DESC",
    ) else {
        return vec![];
    };
    stmt.query_map([], |r| {
        let manifest: String = r.get(4)?;
        let imports: String = r.get(5)?;
        Ok(Version {
            app_id: r.get(0)?,
            version: r.get(1)?,
            name: r.get(2)?,
            author: r.get(3)?,
            manifest: Manifest::parse(&manifest).ok(),
            imports: serde_json::from_str(&imports).unwrap_or_default(),
            status: r.get(6)?,
            reason: r.get(7)?,
            deployed_by: r.get(8)?,
            deployed_at: r.get(9)?,
            id: r.get(10)?,
            review: r.get::<_, Option<String>>(11)?.and_then(|t| serde_json::from_str(&t).ok()),
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn events(db: &Connection, limit: usize) -> Vec<Event> {
    let Ok(mut stmt) = db.prepare(
        "SELECT id, at, app_id, version, kind, message, actor FROM deploy_events ORDER BY id DESC LIMIT ?1",
    ) else {
        return vec![];
    };
    stmt.query_map([limit as i64], |r| {
        Ok(Event {
            id: r.get(0)?,
            at: r.get(1)?,
            app_id: r.get(2)?,
            version: r.get(3)?,
            kind: r.get(4)?,
            message: r.get(5)?,
            actor: r.get(6)?,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

/// The last event the platform team marked as seen.
fn seen_up_to(db: &Connection) -> i64 {
    db.query_row("SELECT value FROM settings WHERE key = 'problems_seen'", [], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

pub fn mark_seen(db: &Connection) {
    let last: i64 = db.query_row("SELECT COALESCE(MAX(id), 0) FROM deploy_events", [], |r| r.get(0)).unwrap_or(0);
    let _ = db.execute(
        "INSERT INTO settings (key, value) VALUES ('problems_seen', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [last.to_string()],
    );
}

/// Problems since they were last marked as seen, one per app, version and kind.
struct Problem<'a> {
    latest: &'a Event,
    count: usize,
}

fn problems(events: &[Event], seen: i64) -> Vec<Problem<'_>> {
    let mut out: Vec<Problem> = Vec::new();
    for e in events.iter().filter(|e| e.id > seen && PROBLEMS.contains(&e.kind.as_str())) {
        match out.iter_mut().find(|p| p.latest.app_id == e.app_id && p.latest.version == e.version && p.latest.kind == e.kind) {
            Some(p) => p.count += 1,
            None => out.push(Problem { latest: e, count: 1 }),
        }
    }
    out
}

fn grants(v: &Version) -> Vec<String> {
    v.manifest.as_ref().map(|m| m.capabilities.grant.clone()).unwrap_or_default()
}

/// "3 min ago" for a timestamp written by `db::now`.
fn ago(at: &str) -> String {
    let Ok(t) = NaiveDateTime::parse_from_str(at, "%Y-%m-%d %H:%M:%S") else { return at.to_string() };
    let secs = (chrono::Local::now().naive_local() - t).num_seconds().max(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => t.format("%-d %b, %H:%M").to_string(),
    }
}

/// "Bob's agent" for "agent:claude (asked by Bob, Product)".
fn short_author(author: &str) -> String {
    match author.split_once("asked by ") {
        Some((_, rest)) => format!("{}'s agent", rest.split([',', ')']).next().unwrap_or(rest).trim()),
        None => author.to_string(),
    }
}

pub fn page(db: &Connection, policy: &Policy, user: &User, view: Option<&str>, error: Option<&str>) -> Markup {
    let view = match view {
        Some("activity") => "activity",
        Some("access") => "access",
        _ => "apps",
    };
    let all = versions(db);
    let events = events(db, ALL_EVENTS);
    let seen = seen_up_to(db);

    let problems = problems(&events, seen);
    let headline = match problems.len() {
        0 => "Nothing went wrong. Every app runs with exactly what its manifest declares.".to_string(),
        1 => "One thing went wrong.".to_string(),
        n => format!("{n} things went wrong."),
    };
    let refresh = match view {
        "apps" => "/admin/apps".to_string(),
        v => format!("/admin/apps?view={v}"),
    };

    let body = html! {
        div #admin hx-get=(refresh) hx-trigger="every 3s" hx-select="#admin" hx-swap="outerHTML" {
            header.page-header {
                h1 { "Apps in Roomer" }
                p.subtitle { (headline) }
            }
            nav.tabs aria-label="Apps in Roomer" {
                a href="/admin/apps" aria-current=[(view == "apps").then_some("page")] { "Apps" }
                a href="/admin/apps?view=activity" aria-current=[(view == "activity").then_some("page")] { "Activity" }
                a href="/admin/apps?view=access" aria-current=[(view == "access").then_some("page")] { "Access" }
            }
            @if let Some(e) = error { p.error { (e) } }
            @match view {
                "activity" => {
                    section.card aria-label="Activity" { (event_table(&events)) }
                }
                "access" => { (access_policy(policy)) }
                _ => {
                    @if !problems.is_empty() { (needs_you(&all, &problems)) }
                    (app_strip(&all))
                    (live_apps(&all, &events, &crate::usage::last_hour_by_app(db)))
                    section.activity aria-labelledby="recent" {
                        div.section-head {
                            h2 #recent { "Recent activity" }
                            a href="/admin/apps?view=activity" { "All activity" }
                        }
                        div.card { (event_table(&events[..events.len().min(RECENT_EVENTS)])) }
                    }
                }
            }
        }
    };
    views::layout("Apps in Roomer", Some(user), "/admin/apps", body, vec![])
}

/// Problems since they were last marked as seen.
fn needs_you(all: &[Version], problems: &[Problem]) -> Markup {
    html! {
        section.needs-you aria-labelledby="needs-you" {
            h2 #needs-you { "Needs a look" }
            div.problems {
                ul {
                    @for p in problems {
                        li {
                            span.problem-mark aria-hidden="true" { "!" }
                            span.problem-text {
                                strong { (problem_title(&p.latest.kind, p.count)) }
                                " " (app_name(all, &p.latest.app_id)) " v" (p.latest.version) ". "
                                span.muted { (p.latest.message) }
                            }
                            span.problem-when { (ago(&p.latest.at)) }
                            @if matches!(p.latest.kind.as_str(), "over-budget" | "crashed" | "failed") {
                                a href=(format!("/admin/usage/{}", p.latest.app_id)) { "Usage" }
                            }
                        }
                    }
                }
                form method="post" action="/admin/seen" {
                    button.btn.small type="submit" { "Mark as seen" }
                }
            }
        }
    }
}

fn problem_title(kind: &str, count: usize) -> String {
    let times = if count > 1 { format!(" {count} times") } else { String::new() };
    match kind {
        "refused" => "Deploy refused:".into(),
        "review-failed" => "Failed app review, still live:".into(),
        "flagged" => "Live with review findings:".into(),
        "denied" => format!("Access denied{times}:"),
        "over-budget" => format!("Stopped{times} for going over budget:"),
        "crashed" => format!("Crashed{times}:"),
        "failed" => "Rolled back automatically:".into(),
        other => format!("{other}:"),
    }
}

fn app_name<'a>(all: &'a [Version], app_id: &'a str) -> &'a str {
    all.iter().find(|v| v.app_id == app_id).map_or(app_id, |v| v.name.as_str())
}

/// The icon of an app without its own: a box.
const BOX_ICON: &str = "M21 8 12 3 3 8v8l9 5 9-5ZM3 8l9 5 9-5M12 13v8";

/// A 24×24 line icon (an SVG path) per app.
fn icon(app_id: &str) -> &'static str {
    match app_id {
        "booking-grid" => "M3 5h18v16H3zM3 10h18M8 3v4M16 3v4M9 10v11M15 10v11M3 15.5h18",
        "pod-admin" => "M4 6h16M4 12h16M4 18h16M8 4v4M16 10v4M11 16v4",
        "pod-stats" => "M4 20h16M7 20v-6M12 20V8M17 20v-10",
        "quick-book" => "M13 3 5 14h6l-1 7 8-11h-6Z",
        _ => BOX_ICON,
    }
}

/// Every app as an icon, like a Finder window: live apps first, then the
/// ones with nothing live, greyed. An icon jumps to the app's row.
fn app_strip(all: &[Version]) -> Markup {
    let mut apps: Vec<(&Version, Option<&Version>)> = Vec::new();
    for v in all {
        if !apps.iter().any(|(a, _)| a.app_id == v.app_id) {
            let live = all.iter().find(|o| o.app_id == v.app_id && o.status == "active");
            apps.push((v, live));
        }
    }
    apps.sort_by_key(|(v, live)| (live.is_none(), v.name.to_lowercase()));
    let live = apps.iter().filter(|(_, l)| l.is_some()).count();

    html! {
        section.app-strip aria-label="Apps" {
            div.app-strip-head {
                strong { (apps.len()) @if apps.len() == 1 { " app" } @else { " apps" } }
                @if !apps.is_empty() { span.muted { "· " (live) " live" } }
            }
            @if apps.is_empty() {
                p.muted.app-strip-empty { "No app is deployed." }
            } @else {
                div.app-icons {
                    @for (v, current) in &apps {
                        @let description = current.and_then(|c| c.manifest.as_ref()).map(|m| m.app.description.as_str()).unwrap_or("");
                        a.app-icon.app-icon-off[current.is_none()] href=(format!("#app-{}", v.app_id)) title=(description) {
                            span.app-icon-glyph {
                                svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4"
                                    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
                                    path d=(icon(&v.app_id)) {}
                                }
                                @match current {
                                    Some(c) => span.app-icon-tag { "v" (c.version) },
                                    None => span.app-icon-tag { "off" },
                                }
                            }
                            span.app-icon-name { (v.name) }
                        }
                    }
                }
            }
        }
    }
}

/// One row per app: what is live, where it shows, what it may do.
fn live_apps(all: &[Version], events: &[Event], usage: &crate::usage::LastHour) -> Markup {
    let mut app_ids: Vec<(&str, &str)> = Vec::new();
    for v in all {
        if !app_ids.iter().any(|(id, _)| *id == v.app_id) {
            app_ids.push((&v.app_id, &v.name));
        }
    }
    app_ids.sort_by_key(|(_, name)| name.to_lowercase());

    html! {
        section.live-apps aria-labelledby="live-apps" {
            div.section-head { h2 #live-apps { "Apps" } }
            @if app_ids.is_empty() {
                div.card { p.muted { "No apps deployed yet. Run " code { "make deploy" } "." } }
            } @else {
                div.card.app-list {
                    div.app-row.app-row-head aria-hidden="true" {
                        span { "App" } span { "Where" } span { "May" } span { "Last hour" } span { "Last change" } span {}
                    }
                    @for (app_id, name) in &app_ids {
                        @let history: Vec<&Version> = all.iter().filter(|v| v.app_id == *app_id).collect();
                        @let current = history.iter().copied().find(|v| v.status == "active");
                        @let last = events.iter().find(|e| e.app_id == *app_id && DEPLOYS.contains(&e.kind.as_str()));
                        @let back_to = current.and_then(|c| history.iter().copied().find(|v| v.status == "retired" && v.id < c.id));
                        div.app id=(format!("app-{app_id}")) {
                            div.app-row {
                                div.app-name {
                                    strong { (name) }
                                    @match current {
                                        Some(c) => {
                                            span.badge.badge-active { "v" (c.version) " live" }
                                            (review_badge(c.review.as_ref()))
                                        }
                                        None => span.badge.badge-retired { "not live" },
                                    }
                                }
                                div { @if let Some(m) = current.and_then(|c| c.manifest.as_ref()) { (slots(m)) } }
                                div.caps.small {
                                    @if let Some(c) = current { @for g in by_risk(grants(c)) { (cap_chip(&g)) } }
                                }
                                div { (usage.sparkline(app_id)) }
                                div.last-change {
                                    @if let Some(e) = last {
                                        span { (change_text(e)) } span.muted { (ago(&e.at)) }
                                    }
                                }
                                div.app-actions {
                                    @if let Some(v) = back_to {
                                        form method="post" action=(format!("/admin/apps/{}/{}/activate", v.app_id, v.version)) {
                                            button.btn.small type="submit" { "Roll back to v" (v.version) }
                                        }
                                    }
                                }
                            }
                            details.history id=(format!("history-{app_id}")) hx-preserve {
                                summary { (history.len()) @if history.len() == 1 { " version" } @else { " versions" } }
                                (version_history(&history, current))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A deploy event as a short line for the "Last change" column.
fn change_text(e: &Event) -> String {
    let v = &e.version;
    match e.kind.as_str() {
        "deployed" => format!("v{v} deployed by {}", short_author(&e.actor)),
        "needs-approval" => format!("v{v} waits for you"),
        "auto-approved" => format!("v{v} auto-approved"),
        "approved" => format!("v{v} approved by {}", e.actor),
        "rolled-back" => format!("Rolled back to v{v} by {}", e.actor),
        "auto-rolled-back" => format!("Rolled back to v{v} automatically"),
        "refused" => format!("v{v} refused"),
        "preinstalled" => format!("v{v} came with Roomer"),
        "reviewed" => format!("v{v} passed review"),
        "review-failed" => format!("v{v} failed review"),
        "flagged" => format!("v{v} live, review warns"),
        "rejected" => format!("v{v} rejected by {}", e.actor),
        "failed" => format!("v{v} failed and was rolled back"),
        other => format!("v{v} {other}"),
    }
}

/// Every version of one app, newest first, with what it changed.
fn version_history(history: &[&Version], current: Option<&Version>) -> Markup {
    html! {
        ol.version-list {
            @for (i, v) in history.iter().enumerate() {
                // Compare with the next older version that got past the sandbox.
                @let before = history[i + 1..].iter().copied().find(|o| o.status != "refused");
                li class=(format!("row-{}", v.status)) {
                    span.mono { "v" (v.version) }
                    (status_badge(&v.status))
                    span.version-change {
                        (version_change(v, before))
                        @if matches!(v.status.as_str(), "refused" | "failed") {
                            @if let Some(r) = &v.reason { span.refusal { (r) } }
                        }
                        (review_detail(v))
                        @if v.status == "refused" {
                            @let used = crate::runtime::used_capabilities(&v.imports);
                            @if !used.is_empty() { span.muted.small-text { "Code actually uses: " (used.join(", ")) } }
                        }
                    }
                    span.muted {
                        (short_author(&v.author))
                        @if !v.author.starts_with(&v.deployed_by) { " via " (v.deployed_by) }
                        ", " (ago(&v.deployed_at))
                    }
                    span.app-actions {
                        @if matches!(v.status.as_str(), "retired" | "pending") {
                            form method="post" action=(format!("/admin/apps/{}/{}/activate", v.app_id, v.version)) {
                                @let older = current.is_some_and(|c| v.id < c.id);
                                button.btn.small type="submit" { @if older { "Roll back to this" } @else { "Make live again" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What a version changed compared with the one before it.
fn version_change(v: &Version, before: Option<&Version>) -> String {
    let Some(before) = before else { return "First version".into() };
    let (new, old) = (grants(v), grants(before));
    let mut parts: Vec<String> = new.iter().filter(|g| !old.contains(g)).map(|g| format!("+{g}")).collect();
    parts.extend(old.iter().filter(|g| !new.contains(g)).map(|g| format!("−{g}")));
    if let (Some(a), Some(b)) = (&v.manifest, &before.manifest) {
        if a.limits.io_per_request != b.limits.io_per_request {
            parts.push(format!("io {} → {}", b.limits.io_per_request, a.limits.io_per_request));
        }
        if a.slots != b.slots {
            parts.push("shows up in new places".into());
        }
    }
    if parts.is_empty() { "Same permissions".into() } else { parts.join(", ") }
}

fn event_table(events: &[Event]) -> Markup {
    html! {
        @if events.is_empty() {
            p.muted { "Nothing yet." }
        } @else {
            table.events {
                tbody {
                    @for e in events {
                        tr {
                            td.muted.nowrap { (ago(&e.at)) }
                            td { (event_badge(&e.kind)) }
                            td.mono.nowrap { (e.app_id) " v" (e.version) }
                            td { (e.message) }
                        }
                    }
                }
            }
        }
    }
}

/// Who may use which capability. Admins inherit everything members may do.
fn access_policy(policy: &Policy) -> Markup {
    let rules = policy.role_rules();
    let has = |role: &str, cap: &str| rules.iter().any(|(r, c)| r == role && c == cap);
    html! {
        div.card {
            p.muted {
                "Casbin checks every platform call: the app must be granted the capability by an approved manifest, "
                "and the signed-in user's role must allow it."
            }
            table.policy {
                thead { tr { th { "Capability" } th { "Risk" } th { "member" } th { "admin" } } }
                tbody {
                    @for (cap, text) in CAPABILITIES {
                        tr {
                            td { (cap_chip(cap)) span.muted { " " (text) } }
                            td.muted.nowrap { (risk(cap).label()) }
                            td { @if has("member", cap) { span.yes { "✓" } } @else { span.no { "—" } } }
                            td { @if has("member", cap) || has("admin", cap) { span.yes { "✓" } } @else { span.no { "—" } } }
                        }
                    }
                }
            }
            details.policy-raw {
                summary { "Casbin policy" }
                pre { code { (policy.dump()) } }
            }
        }
    }
}

/// Where the app shows up.
fn slots(m: &Manifest) -> Markup {
    html! {
        p.slots {
            @for r in &m.slots.routes { span.slot { "page " code { (r) } } }
            @for w in &m.slots.widgets { span.slot { "widget " code { (w) } } }
            @if let Some(label) = &m.slots.nav { span.slot { "nav " strong { (label) } } }
        }
    }
}

/// A capability as a pill coloured by how much harm it can do.
fn cap_chip(cap: &str) -> Markup {
    let r = risk(cap);
    html! { span class=(format!("chip {}", r.class())) title=(format!("{}. {}", describe(cap), r.label())) { (cap) } }
}

/// Riskiest capabilities first, so they are what the eye lands on.
fn by_risk(mut caps: Vec<String>) -> Vec<String> {
    caps.sort_by_key(|c| std::cmp::Reverse(risk(c) as u8));
    caps
}

/// The review verdict of a live version.
fn review_badge(review: Option<&crate::review::Review>) -> Markup {
    match review.map(|r| r.verdict.as_str()) {
        None => html! { span.badge.badge-retired title="Installed before app review; deploy it to review it" { "not reviewed" } },
        Some("pass") => html! { span.badge.badge-active { "review passed" } },
        Some("warn") => html! { span.badge.badge-pending title=(review.map(|r| r.summary()).unwrap_or_default()) { "review warns" } },
        Some(_) => html! { span.badge.badge-refused title=(review.map(|r| r.summary()).unwrap_or_default()) { "review failed" } },
    }
}

/// What app review found in a version.
fn review_detail(v: &Version) -> Markup {
    html! {
        @match &v.review {
            None => @if v.deployed_by == "roomer" { span.muted.small-text { "Came with Roomer, installed before app review" } },
            Some(r) => {
                span.muted.small-text { "App review: " (r.verdict) ", " (r.runs) " runs" }
                @if !r.findings.is_empty() {
                    ul.review-findings {
                        @for f in &r.findings {
                            li { span class=(format!("badge badge-{}", if f.severity == "fail" { "refused" } else { "pending" })) { (f.severity) } " " code { (f.kind) } " " (f.title) }
                        }
                    }
                }
                span.muted.small-text { "Details: " code { "appfuzz explain" } " in " code { (r.dir) } }
            }
        }
    }
}

fn status_badge(status: &str) -> Markup {
    let label = match status {
        "active" => "live",
        "pending" => "not live",
        "retired" => "previous",
        "refused" => "refused",
        "rejected" => "rejected",
        "failed" => "failed",
        other => other,
    };
    html! { span class=(format!("badge badge-{status}")) { (label) } }
}

fn event_badge(kind: &str) -> Markup {
    let class = match kind {
        "deployed" | "auto-approved" | "approved" => "badge-active",
        "needs-approval" => "badge-pending",
        "refused" | "review-failed" | "crashed" | "denied" | "over-budget" | "failed" => "badge-refused",
        "flagged" | "preinstalled" => "badge-pending",
        "reviewed" => "badge-active",
        "rolled-back" | "auto-rolled-back" => "badge-rollback",
        _ => "badge-retired",
    };
    html! { span class=(format!("badge {class}")) { (kind) } }
}
