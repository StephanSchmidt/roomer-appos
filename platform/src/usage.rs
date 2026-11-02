//! Usage: what each app request costs, measured by the platform, and the
//! screens that show it: a dashboard over all apps and a page per app.
//!
//! Charts are server-rendered SVG. The CSP allows no inline scripts, so
//! hover details are SVG `<title>` tooltips.

use std::time::Duration;

use chrono::TimeZone;
use maud::{Markup, html};
use rusqlite::{Connection, params};

use crate::db::User;
use crate::manifest::{DEFAULT_IO_PER_REQUEST, Manifest};
use crate::platform::{PLATFORM_TIME_PER_REQUEST, Platform};
use crate::runtime::{FUEL_LIMIT, Usage};
use crate::views;

/// Requests kept per app.
const KEEP_PER_APP: i64 = 5000;
/// Requests shown in the per-request chart.
const TIMELINE_REQUESTS: usize = 200;
/// Minutes shown in the requests-per-minute chart.
const MINUTES: i64 = 30;

pub fn record(db: &Connection, app_id: &str, version: &str, slot: &str, outcome: &str, usage: Option<&Usage>, total: Duration) {
    let (io, platform_us, fuel) =
        usage.map(|u| (u.io_units, u.platform_time.as_micros() as i64, u.fuel as i64)).unwrap_or((0, 0, 0));
    let _ = db.execute(
        "INSERT INTO request_stats (at, app_id, version, slot, outcome, io_units, platform_us, fuel, total_us)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![now_ms(), app_id, version, slot, outcome, io, platform_us, fuel, total.as_micros() as i64],
    );
    let _ = db.execute(
        "DELETE FROM request_stats WHERE app_id = ?1 AND id <=
         (SELECT id FROM request_stats WHERE app_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
        params![app_id, KEEP_PER_APP],
    );
}

fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

struct Sample {
    at: i64,
    app_id: String,
    version: String,
    slot: String,
    outcome: String,
    io: f64,
    platform_ms: f64,
    cpu_m: f64,
    total_ms: f64,
}

impl Sample {
    fn failed(&self) -> bool {
        self.outcome != "ok"
    }
    /// Busy requests were turned away before running, so they cost nothing.
    fn ran(&self) -> bool {
        self.outcome != "busy"
    }
}

/// Recorded requests, oldest first.
fn samples(db: &Connection, app_id: Option<&str>) -> Vec<Sample> {
    let Ok(mut stmt) = db.prepare(
        "SELECT at, app_id, version, slot, outcome, io_units, platform_us, fuel, total_us FROM request_stats
         WHERE ?1 IS NULL OR app_id = ?1 ORDER BY id",
    ) else {
        return vec![];
    };
    stmt.query_map([app_id], |r| {
        Ok(Sample {
            at: r.get(0)?,
            app_id: r.get(1)?,
            version: r.get(2)?,
            slot: r.get(3)?,
            outcome: r.get(4)?,
            io: r.get::<_, i64>(5)? as f64,
            platform_ms: r.get::<_, i64>(6)? as f64 / 1000.0,
            cpu_m: r.get::<_, i64>(7)? as f64 / 1e6,
            total_ms: r.get::<_, i64>(8)? as f64 / 1000.0,
        })
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

/// Every version of an app that was ever live or pending, newest first,
/// with its status and manifest.
fn versions(db: &Connection, app_id: &str) -> Vec<(String, String, Option<Manifest>)> {
    let Ok(mut stmt) = db.prepare(
        "SELECT version, status, manifest FROM app_versions
         WHERE app_id = ?1 AND status NOT IN ('refused', 'rejected') ORDER BY id DESC",
    ) else {
        return vec![];
    };
    stmt.query_map([app_id], |r| Ok((r.get(0)?, r.get(1)?, Manifest::parse(&r.get::<_, String>(2)?).ok())))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

// ---- numbers ---------------------------------------------------------------

struct Summary {
    min: f64,
    median: f64,
    p95: f64,
    max: f64,
}

fn summarize(values: impl Iterator<Item = f64>) -> Option<Summary> {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Some(Summary { min: v[0], median: at(0.5), p95: at(0.95), max: v[v.len() - 1] })
}

/// One measured quantity, with its budget if it has one.
struct Metric {
    label: &'static str,
    unit: &'static str,
    hint: &'static str,
    value: fn(&Sample) -> f64,
    limit: Option<f64>,
    decimals: usize,
}

fn metrics(io_limit: u32) -> [Metric; 4] {
    [
        Metric {
            label: "I/O",
            unit: "units",
            hint: "Platform calls, weighted: a read costs 1, a write 5",
            value: |s| s.io,
            limit: Some(io_limit as f64),
            decimals: 0,
        },
        Metric {
            label: "Platform time",
            unit: "ms",
            hint: "Time spent inside platform calls, e.g. waiting on the database",
            value: |s| s.platform_ms,
            limit: Some(PLATFORM_TIME_PER_REQUEST.as_secs_f64() * 1000.0),
            decimals: 2,
        },
        Metric {
            label: "CPU",
            unit: "M instr",
            hint: "WebAssembly instructions the app executed (fuel)",
            value: |s| s.cpu_m,
            limit: Some(FUEL_LIMIT as f64 / 1e6),
            decimals: 1,
        },
        Metric {
            label: "Total time",
            unit: "ms",
            hint: "Wall time of the whole request, including instantiating the app",
            value: |s| s.total_ms,
            limit: None,
            decimals: 2,
        },
    ]
}

fn num(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

/// A round axis maximum at or above `v`, and its tick step (about 4 ticks).
fn nice_axis(v: f64) -> (f64, f64) {
    let v = if v > 0.0 { v } else { 1.0 };
    let raw = v / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = mag
        * match raw / mag {
            n if n <= 1.0 => 1.0,
            n if n <= 2.0 => 2.0,
            n if n <= 5.0 => 5.0,
            _ => 10.0,
        };
    ((v / step).ceil() * step, step)
}

fn time_of(at: i64) -> String {
    chrono::Local.timestamp_millis_opt(at).single().map(|t| t.format("%H:%M:%S").to_string()).unwrap_or_default()
}

fn f(v: f64) -> String {
    format!("{v:.1}")
}

// ---- charts ----------------------------------------------------------------

/// Where min..max sits against the budget: the band is the range, the dot
/// the median, the tick the 95th percentile, the dashed line the limit.
fn range_bar(s: &Summary, m: &Metric) -> Markup {
    let scale = m.limit.unwrap_or(0.0).max(s.max).max(f64::MIN_POSITIVE);
    let (w, x0) = (232.0, 4.0);
    let x = |v: f64| x0 + v / scale * w;
    let tip = format!(
        "{}: min {} · median {} · p95 {} · max {}{} {}",
        m.label,
        num(s.min, m.decimals),
        num(s.median, m.decimals),
        num(s.p95, m.decimals),
        num(s.max, m.decimals),
        m.limit.map(|l| format!(" · limit {}", num(l, 0))).unwrap_or_default(),
        m.unit
    );
    html! {
        svg.viz.range viewBox="0 0 240 22" width="240" height="22" role="img" aria-label=(tip) {
            title { (tip) }
            rect.range-track x=(f(x0)) y="8" width=(f(w)) height="6" rx="3" {}
            rect.range-band x=(f(x(s.min))) y="8" width=(f((x(s.max) - x(s.min)).max(2.0))) height="6" rx="3" {}
            line.range-p95 x1=(f(x(s.p95))) x2=(f(x(s.p95))) y1="5" y2="17" {}
            circle.range-median cx=(f(x(s.median))) cy="11" r="5" {}
            @if let Some(l) = m.limit {
                line.limit x1=(f(x(l))) x2=(f(x(l))) y1="1" y2="21" {}
            }
        }
    }
}

/// The min / median / p95 / max table, one row per metric. Doubles as the
/// table view of the range bars.
fn metrics_table(samples: &[&Sample], io_limit: u32) -> Markup {
    let ran: Vec<&&Sample> = samples.iter().filter(|s| s.ran()).collect();
    html! {
        table.usage-table {
            thead {
                tr { th { "Per request" } th.num { "min" } th.num { "median" } th.num { "p95" } th.num { "max" } th.num { "limit" } th { "range vs. limit" } }
            }
            tbody {
                @for m in metrics(io_limit) {
                    @if let Some(s) = summarize(ran.iter().map(|x| (m.value)(x))) {
                        tr {
                            td title=(m.hint) { (m.label) " " span.muted { (m.unit) } }
                            td.num { (num(s.min, m.decimals)) }
                            td.num { strong { (num(s.median, m.decimals)) } }
                            td.num { (num(s.p95, m.decimals)) }
                            td.num { (num(s.max, m.decimals)) }
                            td.num.muted { @if let Some(l) = m.limit { (num(l, 0)) } @else { "—" } }
                            td { (range_bar(&s, &m)) }
                        }
                    }
                }
            }
        }
        @if ran.is_empty() { p.muted { "No requests yet." } }
    }
}

/// Chart widths in SVG units, matching the rendered size so text stays 11px.
const FULL: f64 = 1080.0;
const HALF: f64 = 520.0;
const PAD_L: f64 = 44.0;
const PAD_R: f64 = 12.0;
const PAD_T: f64 = 22.0;
const PAD_B: f64 = 26.0;
/// Where the timeline's version labels start.
const TIMELINE_TOP: f64 = 22.0;

/// Horizontal grid lines with labels, for a y axis from 0 to `max`.
fn y_grid(w: f64, max: f64, step: f64, h: f64, unit_decimals: usize) -> Markup {
    let y = |v: f64| PAD_T + (1.0 - v / max) * (h - PAD_T - PAD_B);
    let ticks: Vec<f64> = (0..).map(|i| i as f64 * step).take_while(|v| *v <= max + step / 2.0).collect();
    html! {
        @for t in ticks {
            line class=(if t == 0.0 { "baseline" } else { "grid" }) x1=(f(PAD_L)) x2=(f(w - PAD_R)) y1=(f(y(t))) y2=(f(y(t))) {}
            text.tick x=(f(PAD_L - 8.0)) y=(f(y(t) + 4.0)) text-anchor="end" { (num(t, unit_decimals)) }
        }
    }
}

fn limit_line(w: f64, limit: f64, max: f64, h: f64) -> Markup {
    let y = PAD_T + (1.0 - limit / max) * (h - PAD_T - PAD_B);
    html! {
        line.limit x1=(f(PAD_L)) x2=(f(w - PAD_R)) y1=(f(y)) y2=(f(y)) {}
        text.limit-label x=(f(w - PAD_R)) y=(f(y - 6.0)) text-anchor="end" { "limit " (num(limit, 0)) }
    }
}

/// I/O units of each recent request, in order, against the limit. Version
/// changes are marked; failed requests are red.
fn timeline(w: f64, samples: &[&Sample], limit: f64) -> Markup {
    let ran: Vec<&&Sample> = samples.iter().filter(|s| s.ran()).collect();
    let shown = &ran[ran.len().saturating_sub(TIMELINE_REQUESTS)..];
    if shown.is_empty() {
        return html! { p.muted { "No requests yet." } };
    }
    let h = 260.0;
    let top = shown.iter().map(|s| s.io).fold(limit, f64::max) * 1.08;
    let (max, step) = nice_axis(top);
    let slot_w = (w - PAD_L - PAD_R) / shown.len().max(1) as f64;
    let x = |i: usize| PAD_L + (i as f64 + 0.5) * slot_w;
    let y = |v: f64| PAD_T + (1.0 - v / max) * (h - PAD_T - PAD_B);
    html! {
        svg.viz viewBox=(format!("0 0 {w} {h}")) width="100%" role="img"
            aria-label="I/O units per request, most recent requests, against the limit" {
            (y_grid(w, max, step, h, 0))
            @for (vx, row, version) in version_marks(shown, x, slot_w) {
                line.version-line x1=(f(vx)) x2=(f(vx)) y1=(f(row - 10.0)) y2=(f(h - PAD_B)) {}
                text.version-label x=(f(vx + 4.0)) y=(f(row)) { "v" (version) }
            }
            (limit_line(w, limit, max, h))
            @for (i, s) in shown.iter().enumerate() {
                g.point {
                    title { (time_of(s.at)) " · v" (s.version) " · " (s.slot) " · " (num(s.io, 0)) " I/O units · " (s.outcome) }
                    circle.hit cx=(f(x(i))) cy=(f(y(s.io))) r="9" {}
                    circle class=(if s.failed() { "dot dot-fail" } else { "dot" }) cx=(f(x(i))) cy=(f(y(s.io))) r="4" {}
                }
            }
            text.tick x=(f(PAD_L)) y=(f(h - 6.0)) { "older" }
            text.tick x=(f(w - PAD_R)) y=(f(h - 6.0)) text-anchor="end" { "latest" }
        }
        (legend(true))
    }
}

/// Where each version starts in the timeline: x, label baseline, version.
/// A label too close to the previous one moves down a row.
fn version_marks(shown: &[&&Sample], x: impl Fn(usize) -> f64, slot_w: f64) -> Vec<(f64, f64, String)> {
    let mut marks: Vec<(f64, f64, String)> = Vec::new();
    for (i, s) in shown.iter().enumerate() {
        if i == 0 || shown[i - 1].version != s.version {
            let vx = x(i) - slot_w / 2.0;
            let crowded = marks.last().is_some_and(|(px, row, _)| vx - px < 64.0 && *row == TIMELINE_TOP - 8.0);
            marks.push((vx, if crowded { TIMELINE_TOP + 6.0 } else { TIMELINE_TOP - 8.0 }, s.version.clone()));
        }
    }
    marks
}

/// How the I/O units of all recorded requests are distributed.
fn histogram(w: f64, samples: &[&Sample], limit: f64) -> Markup {
    let values: Vec<f64> = samples.iter().filter(|s| s.ran()).map(|s| s.io).collect();
    if values.is_empty() {
        return html! { p.muted { "No requests yet." } };
    }
    let h = 220.0;
    let top = values.iter().copied().fold(limit, f64::max) * 1.02;
    // Round bins: five per axis step.
    let (x_max, x_step) = nice_axis(top);
    let bin_w = (x_step / 5.0).max(1.0);
    let bins = (x_max / bin_w).ceil() as usize;
    let mut counts = vec![0usize; bins];
    for v in &values {
        counts[((v / bin_w) as usize).min(bins - 1)] += 1;
    }
    let (max, step) = nice_axis(*counts.iter().max().unwrap_or(&1) as f64);
    let x_max = bins as f64 * bin_w;
    let plot = w - PAD_L - PAD_R;
    let x = |v: f64| PAD_L + v / x_max * plot;
    let y = |v: f64| PAD_T + (1.0 - v / max) * (h - PAD_T - PAD_B);
    let base = y(0.0);
    html! {
        svg.viz viewBox=(format!("0 0 {w} {h}")) width="100%" role="img" aria-label="Distribution of I/O units per request" {
            (y_grid(w, max, step, h, 0))
            @for (i, &c) in counts.iter().enumerate() {
                @let (lo, hi) = (i as f64 * bin_w, (i + 1) as f64 * bin_w);
                @if c > 0 {
                    g.col {
                        title { (num(lo, 0)) "–" (num(hi - 1.0, 0)) " I/O units: " (c) @if c == 1 { " request" } @else { " requests" } }
                        rect.hit x=(f(x(lo))) y=(f(PAD_T)) width=(f(x(hi) - x(lo))) height=(f(base - PAD_T)) {}
                        path class=(if lo >= limit { "bar bar-fail" } else { "bar" }) d=(bar_path(x(lo) + 1.0, x(hi) - 1.0, y(c as f64), base)) {}
                    }
                }
            }
            @if limit <= x_max {
                line.limit x1=(f(x(limit))) x2=(f(x(limit))) y1=(f(PAD_T - 8.0)) y2=(f(base)) {}
                text.limit-label x=(f(x(limit) - 6.0)) y=(f(PAD_T - 10.0)) text-anchor="end" { "limit " (num(limit, 0)) }
            }
            @for t in (0..).map(|i| i as f64 * x_step).take_while(|t| *t <= x_max) {
                text.tick x=(f(x(t))) y=(f(h - 8.0)) text-anchor=(if t == 0.0 { "start" } else if t == x_max { "end" } else { "middle" }) {
                    (num(t, 0)) @if t == x_max { " units" }
                }
            }
        }
    }
}

/// A bar with 4px rounded top corners and a square foot on the baseline.
fn bar_path(x0: f64, x1: f64, top: f64, base: f64) -> String {
    let r = 4f64.min((x1 - x0) / 2.0).min(base - top).max(0.0);
    format!(
        "M{} {}V{}Q{} {} {} {}H{}Q{} {} {} {}V{}Z",
        f(x0), f(base), f(top + r), f(x0), f(top), f(x0 + r), f(top), f(x1 - r), f(x1), f(top), f(x1), f(top + r), f(base)
    )
}

/// Requests per minute over the last half hour, failed ones stacked on top.
fn per_minute(w: f64, samples: &[&Sample]) -> Markup {
    let h = 200.0;
    let now = now_ms() / 60_000;
    let mut ok = vec![0usize; MINUTES as usize];
    let mut failed = vec![0usize; MINUTES as usize];
    for s in samples {
        let age = now - s.at / 60_000;
        if (0..MINUTES).contains(&age) {
            let i = (MINUTES - 1 - age) as usize;
            if s.failed() { failed[i] += 1 } else { ok[i] += 1 }
        }
    }
    let (max, step) = nice_axis(ok.iter().zip(&failed).map(|(a, b)| a + b).max().unwrap_or(0).max(1) as f64);
    let slot = (w - PAD_L - PAD_R) / MINUTES as f64;
    let y = |v: f64| PAD_T + (1.0 - v / max) * (h - PAD_T - PAD_B);
    let base = y(0.0);
    let minute = |i: usize| {
        chrono::Local
            .timestamp_opt((now - (MINUTES - 1 - i as i64)) * 60, 0)
            .single()
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_default()
    };
    html! {
        svg.viz viewBox=(format!("0 0 {w} {h}")) width="100%" role="img" aria-label="Requests per minute, last 30 minutes" {
            (y_grid(w, max, step, h, 0))
            @for i in 0..MINUTES as usize {
                @let (x0, x1) = (PAD_L + i as f64 * slot + 2.0, PAD_L + (i + 1) as f64 * slot - 2.0);
                @if ok[i] + failed[i] > 0 {
                    g.col {
                        title { (minute(i)) " · " (ok[i] + failed[i]) " requests" @if failed[i] > 0 { ", " (failed[i]) " failed" } }
                        rect.hit x=(f(x0)) y=(f(PAD_T)) width=(f(x1 - x0)) height=(f(base - PAD_T)) {}
                        @if failed[i] == 0 {
                            path.bar d=(bar_path(x0, x1, y(ok[i] as f64), base)) {}
                        } @else {
                            @if ok[i] > 0 { rect.bar x=(f(x0)) y=(f(y(ok[i] as f64))) width=(f(x1 - x0)) height=(f(base - y(ok[i] as f64))) {} }
                            // 2px gap between the segments.
                            @let seg_base = if ok[i] > 0 { y(ok[i] as f64) - 2.0 } else { base };
                            path.bar.bar-fail d=(bar_path(x0, x1, y((ok[i] + failed[i]) as f64).min(seg_base - 1.0), seg_base)) {}
                        }
                    }
                }
            }
            @for i in (0..MINUTES as usize).step_by(5).chain([MINUTES as usize - 1]) {
                text.tick x=(f(PAD_L + (i as f64 + 0.5) * slot)) y=(f(h - 8.0)) text-anchor="middle" { (minute(i)) }
            }
        }
        (legend(false))
    }
}

fn legend(with_limit: bool) -> Markup {
    html! {
        div.legend {
            span.legend-item { span.swatch.swatch-ok {} "Request" }
            span.legend-item { span.swatch.swatch-fail {} "Failed: over budget, crashed or busy" }
            @if with_limit { span.legend-item { span.swatch.swatch-limit {} "Limit" } }
        }
    }
}

// ---- pages -----------------------------------------------------------------

/// Live apps first, then any app that only has recorded requests.
fn app_list(p: &Platform, all: &[Sample]) -> Vec<(String, String, Option<Manifest>)> {
    let mut apps: Vec<(String, String, Option<Manifest>)> = {
        let live = p.apps.read().unwrap_or_else(|e| e.into_inner());
        live.values().map(|a| (a.manifest.app.id.clone(), a.manifest.app.name.clone(), Some(a.manifest.clone()))).collect()
    };
    for s in all {
        if !apps.iter().any(|(id, _, _)| *id == s.app_id) {
            apps.push((s.app_id.clone(), s.app_id.clone(), None));
        }
    }
    apps.sort_by_key(|(_, name, _)| name.to_lowercase());
    apps
}

fn io_limit(m: Option<&Manifest>) -> u32 {
    m.map(|m| m.limits.io_per_request).unwrap_or(DEFAULT_IO_PER_REQUEST)
}

fn last_hour<'a>(samples: impl Iterator<Item = &'a Sample>) -> Vec<&'a Sample> {
    let since = now_ms() - 3_600_000;
    samples.filter(|s| s.at >= since).collect()
}

fn refreshing(path: &str, body: Markup) -> Markup {
    html! {
        div #usage hx-get=(path) hx-trigger="every 5s" hx-select="#usage" hx-swap="outerHTML" { (body) }
    }
}

pub fn dashboard(p: &Platform, user: &User) -> Markup {
    let all = samples(&p.db(), None);
    let apps = app_list(p, &all);
    let hour = last_hour(all.iter());
    let failed = hour.iter().filter(|s| s.failed()).count();
    // The app closest to its I/O limit, by p95.
    let tightest = apps
        .iter()
        .filter_map(|(id, name, m)| {
            let s = summarize(all.iter().filter(|s| s.app_id == *id && s.ran()).map(|s| s.io))?;
            Some((name, s.p95 / io_limit(m.as_ref()) as f64 * 100.0))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let refs: Vec<&Sample> = all.iter().collect();

    let body = html! {
        header.page-header {
            h1 { "Usage" }
            p.subtitle { "What each app's requests cost, measured by the platform. Apps can't skip or fake this." }
        }
        div.stats {
            div.stat.stat-neutral { span.stat-value { (hour.len()) } span.stat-label { "app requests in the last hour" } }
            div class=(if failed > 0 { "stat stat-refused" } else { "stat" }) {
                span.stat-value { (failed) } span.stat-label { "failed or stopped in the last hour" }
            }
            div.stat.stat-neutral {
                @if let Some((name, pct)) = tightest {
                    span.stat-value { (num(pct, 0)) "%" }
                    span.stat-label { "of its I/O budget used at p95 by " (name) ", the closest to its limit" }
                } @else {
                    span.stat-value { "—" } span.stat-label { "of I/O budget used" }
                }
            }
        }
        div.card {
            h3.chart-title { "Requests per minute, all apps" }
            (per_minute(FULL, &refs))
        }
        h2.section-title { "Apps" }
        @for (id, name, m) in &apps {
            @let mine: Vec<&Sample> = all.iter().filter(|s| s.app_id == *id).collect();
            @let fails = mine.iter().filter(|s| s.failed()).count();
            div.card.app-card {
                div.app-head {
                    div {
                        h3 { a href=(format!("/admin/usage/{id}")) { (name) } " " span.muted.mono { (id) } }
                        p.muted {
                            (mine.len()) " requests recorded"
                            @if fails > 0 { " · " span.fail-text { (fails) " failed" } }
                            @if let Some(m) = m { " · live: v" (m.app.version) }
                        }
                    }
                    a.btn.small href=(format!("/admin/usage/{id}")) { "Details" }
                }
                (metrics_table(&mine, io_limit(m.as_ref())))
            }
        }
        @if apps.is_empty() { div.card { p.muted { "No apps yet." } } }
    };
    views::layout("Usage", Some(user), "/admin/usage", refreshing("/admin/usage", body), vec![])
}

pub fn app_page(p: &Platform, user: &User, app_id: &str) -> Option<Markup> {
    let all = samples(&p.db(), Some(app_id));
    let versions = versions(&p.db(), app_id);
    if all.is_empty() && versions.is_empty() {
        return None;
    }
    let live = p.apps.read().unwrap_or_else(|e| e.into_inner()).get(app_id).map(|a| a.manifest.clone());
    let name = live
        .as_ref()
        .or_else(|| versions.iter().find_map(|(_, _, m)| m.as_ref()))
        .map(|m| m.app.name.clone())
        .unwrap_or_else(|| app_id.to_string());
    let limit = io_limit(live.as_ref());
    let refs: Vec<&Sample> = all.iter().collect();
    let failed = all.iter().filter(|s| s.failed()).count();
    let io = summarize(all.iter().filter(|s| s.ran()).map(|s| s.io));
    let path = format!("/admin/usage/{app_id}");

    let body = html! {
        header.page-header {
            p.crumb { a href="/admin/usage" { "← Usage" } }
            h1 { (name) " " span.muted.mono.h1-id { (app_id) } }
            p.subtitle {
                @if let Some(m) = &live { "Live: v" (m.app.version) " · I/O limit " (m.limits.io_per_request) " units per request" }
                @else { "Not live" }
            }
        }
        div.stats.stats-4 {
            div.stat.stat-neutral { span.stat-value { (all.len()) } span.stat-label { "requests recorded" } }
            div class=(if failed > 0 { "stat stat-refused" } else { "stat" }) { span.stat-value { (failed) } span.stat-label { "failed or stopped" } }
            div.stat.stat-neutral {
                span.stat-value { @if let Some(s) = &io { (num(s.median, 0)) } @else { "—" } }
                span.stat-label { "median I/O units per request" }
            }
            div.stat.stat-neutral {
                span.stat-value { @if let Some(s) = &io { (num(s.max, 0)) } @else { "—" } " / " (limit) }
                span.stat-label { "peak I/O vs. limit" }
            }
        }
        div.card {
            h3.chart-title { "I/O units per request" span.muted { " · last " (TIMELINE_REQUESTS) " requests" } }
            (timeline(FULL, &refs, limit as f64))
        }
        div.chart-pair {
            div.card {
                h3.chart-title { "Distribution of I/O units" }
                (histogram(HALF, &refs, limit as f64))
            }
            div.card {
                h3.chart-title { "Requests per minute" }
                (per_minute(HALF, &refs))
            }
        }
        h2.section-title { "By version" }
        @for (version, status, m) in &versions {
            @let mine: Vec<&Sample> = all.iter().filter(|s| s.version == *version).collect();
            @let fails = mine.iter().filter(|s| s.failed()).count();
            div.card {
                div.app-head {
                    h3 { "v" (version) }
                    span class=(format!("badge badge-{status}")) { (status_label(status)) }
                }
                p.muted {
                    (mine.len()) " requests"
                    @if fails > 0 { " · " span.fail-text { (fails) " failed" } }
                    " · I/O limit " (io_limit(m.as_ref()))
                }
                @if !mine.is_empty() { (metrics_table(&mine, io_limit(m.as_ref()))) }
            }
        }
    };
    Some(views::layout(&format!("{name} usage"), Some(user), "/admin/usage", refreshing(&path, body), vec![]))
}

fn status_label(status: &str) -> &str {
    match status {
        "active" => "live",
        "pending" => "not live",
        "retired" => "previous",
        "failed" => "failed, rolled back",
        other => other,
    }
}

/// Buckets in the last-hour sparklines on the admin screen, two minutes each.
const SPARK_BUCKETS: i64 = 30;
const SPARK_BUCKET_MS: i64 = 120_000;

/// Each app's requests over the last hour, bucketed for a sparkline.
pub struct LastHour {
    /// app id → (ok, failed) per bucket, oldest first.
    apps: std::collections::HashMap<String, (Vec<usize>, Vec<usize>)>,
    /// The busiest bucket of any app, so all sparklines share one scale.
    max: usize,
}

pub fn last_hour_by_app(db: &Connection) -> LastHour {
    let now = now_ms();
    let since = now - SPARK_BUCKETS * SPARK_BUCKET_MS;
    let mut apps: std::collections::HashMap<String, (Vec<usize>, Vec<usize>)> = Default::default();
    if let Ok(mut stmt) = db.prepare("SELECT at, app_id, outcome FROM request_stats WHERE at >= ?1") {
        let rows = stmt.query_map([since], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)));
        for (at, app_id, outcome) in rows.into_iter().flatten().filter_map(Result::ok) {
            let i = ((at - since) / SPARK_BUCKET_MS).clamp(0, SPARK_BUCKETS - 1) as usize;
            let (ok, failed) = apps
                .entry(app_id)
                .or_insert_with(|| (vec![0; SPARK_BUCKETS as usize], vec![0; SPARK_BUCKETS as usize]));
            if outcome == "ok" { ok[i] += 1 } else { failed[i] += 1 }
        }
    }
    let max = apps.values().flat_map(|(ok, failed)| ok.iter().zip(failed).map(|(a, b)| a + b)).max().unwrap_or(0);
    LastHour { apps, max }
}

impl LastHour {
    /// A small bar chart of one app's last hour, linking to its usage page.
    pub fn sparkline(&self, app_id: &str) -> Markup {
        let Some((ok, failed)) = self.apps.get(app_id) else {
            return html! { span.spark-empty { "No requests in the last hour" } };
        };
        let (w, h) = (120.0, 28.0);
        let slot = w / SPARK_BUCKETS as f64;
        let y = |v: usize| h - v as f64 / self.max.max(1) as f64 * (h - 2.0);
        let total: usize = ok.iter().sum::<usize>() + failed.iter().sum::<usize>();
        let fails: usize = failed.iter().sum();
        let ago = |i: usize| (SPARK_BUCKETS as usize - i) * (SPARK_BUCKET_MS / 60_000) as usize;
        html! {
            a.spark href=(format!("/admin/usage/{app_id}")) {
                svg.viz.spark-chart viewBox=(format!("0 0 {w} {h}")) width=(w) height=(h) role="img"
                    aria-label=(format!("{total} requests in the last hour, {fails} failed")) {
                    line.baseline x1="0" x2=(w) y1=(h - 0.5) y2=(h - 0.5) {}
                    @for i in 0..SPARK_BUCKETS as usize {
                        @if ok[i] + failed[i] > 0 {
                            @let (x, bw) = (i as f64 * slot + 0.5, slot - 1.0);
                            g.col {
                                title { (ago(i)) " min ago: " (ok[i] + failed[i]) " requests" @if failed[i] > 0 { ", " (failed[i]) " failed" } }
                                rect.hit x=(f(x)) y="0" width=(f(bw)) height=(h) {}
                                @if ok[i] > 0 { rect.bar x=(f(x)) y=(f(y(ok[i]))) width=(f(bw)) height=(f(h - y(ok[i]))) {} }
                                @if failed[i] > 0 {
                                    @let top = y(ok[i] + failed[i]).min(y(ok[i]) - 1.0);
                                    rect.bar.bar-fail x=(f(x)) y=(f(top)) width=(f(bw)) height=(f(y(ok[i]) - top)) {}
                                }
                            }
                        }
                    }
                }
                span.spark-label {
                    (total) " req"
                    @if fails > 0 { ", " span.fail-text { (fails) " failed" } }
                }
            }
        }
    }
}
