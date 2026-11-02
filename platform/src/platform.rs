//! The platform: shared state, and the implementation of every platform API
//! apps can be granted. Business rules live here, not in the apps.
//!
//! Every call passes one gate, [`Ctx::call`]: it is metered against the
//! request's I/O and time budget, then checked against the access policy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, params};
use wasmtime::Engine;

use crate::manifest::describe;
use crate::policy::Policy;
use crate::runtime::appos::platform::{bookings_read, bookings_write, identity, log, pods_read, pods_write, types};
use crate::runtime::{Ctx, LoadedApp};

pub struct Platform {
    pub db: Mutex<Connection>,
    pub policy: Policy,
    pub engine: Engine,
    /// The active version of every app, by app id.
    pub apps: RwLock<HashMap<String, Arc<LoadedApp>>>,
    /// Recent failed requests per app, for automatic rollback.
    pub failures: Mutex<HashMap<String, Vec<Instant>>>,
    pub deploy_token: String,
    /// How long `appfuzz review` fuzzes each new version.
    pub review_time: String,
}

impl Platform {
    pub fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Rebuilds the top bar from the nav links of the live apps.
    pub fn refresh_nav(&self) {
        let apps = self.apps.read().unwrap_or_else(|e| e.into_inner());
        let mut links: Vec<(&String, (String, String))> =
            apps.iter().filter_map(|(id, a)| Some((id, a.manifest.nav_link()?))).collect();
        links.sort_by(|a, b| a.0.cmp(b.0));
        crate::views::set_nav(links.into_iter().map(|(_, l)| l).collect());
    }
}

/// Bookings start on the hour from 08:00; the last one ends at 17:00.
pub const BOOKABLE_HOURS: std::ops::RangeInclusive<u8> = 8..=16;

/// Time a request may spend inside platform calls. Fuel only counts the
/// app's own instructions, so waiting on the database is budgeted here.
pub const PLATFORM_TIME_PER_REQUEST: Duration = Duration::from_millis(200);

/// What one call costs against the request's I/O budget.
pub const READ: u32 = 1;
pub const WRITE: u32 = 5;

/// Most pods a list returns. Platform APIs never return unbounded results.
pub const MAX_PODS: u32 = 200;

/// Marks traps raised by the platform's budgets, so the shell can tell them
/// apart from other failures.
pub const BUDGET_EXCEEDED: &str = "AppOS budget exceeded";

impl Ctx {
    /// The gate every platform call passes. Charges the call to the request's
    /// I/O budget and time budget (trapping the app if either runs out),
    /// checks the access policy, then runs the call.
    fn call<T>(
        &mut self,
        cap: &str,
        cost: u32,
        denied: impl FnOnce(String) -> T,
        run: impl FnOnce(&mut Self) -> T,
    ) -> wasmtime::Result<T> {
        self.io_used += cost;
        if self.io_used > self.io_limit {
            return Err(wasmtime::format_err!(
                "{BUDGET_EXCEEDED}: more than {} I/O units in one request (last call: {cap})",
                self.io_limit
            ));
        }
        if self.platform_time > PLATFORM_TIME_PER_REQUEST {
            return Err(wasmtime::format_err!(
                "{BUDGET_EXCEEDED}: more than {} ms in platform calls in one request",
                PLATFORM_TIME_PER_REQUEST.as_millis()
            ));
        }
        if let Err(message) = self.check(cap) {
            return Ok(denied(message));
        }
        let start = Instant::now();
        let result = run(self);
        self.platform_time += start.elapsed();
        Ok(result)
    }

    /// Asks the access policy whether this app may use `cap` for this user.
    fn check(&self, cap: &str) -> Result<(), String> {
        let user = self.user.as_ref().map(|u| u.id.as_str()).unwrap_or("anonymous");
        if self.platform.policy.allows(&self.app_id, user, cap) {
            return Ok(());
        }
        let who = self.user.as_ref().map(|u| u.name.as_str()).unwrap_or("Anonymous");
        let message = format!("{who} is not allowed to {}", describe(cap).to_lowercase());
        crate::db::event(
            &self.platform.db(),
            &self.app_id,
            &self.app_version,
            "denied",
            &format!("Policy denied {cap} for {user}: {message}"),
            "casbin",
        );
        Err(message)
    }
}

/// Errors go to apps written for people. Database details stay in the
/// platform's log.
fn internal(message: &str, e: rusqlite::Error) -> String {
    eprintln!("platform error: {message}: {e}");
    format!("{message}; try again")
}

fn valid_date(date: &str) -> Result<(), String> {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|_| ()).map_err(|_| format!("{date:?} is not a date"))
}

impl types::Host for Ctx {}

impl pods_read::Host for Ctx {
    fn all(&mut self) -> wasmtime::Result<Vec<types::Pod>> {
        self.call("pods-read", READ, |_| vec![], |c| {
            let db = c.platform.db();
            let Ok(mut stmt) = db.prepare("SELECT id, name FROM pods ORDER BY id LIMIT ?1") else { return vec![] };
            stmt.query_map([MAX_PODS], |r| Ok(types::Pod { id: r.get(0)?, name: r.get(1)? }))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        })
    }
}

impl pods_write::Host for Ctx {
    fn create(&mut self, name: String) -> wasmtime::Result<Result<types::Pod, String>> {
        self.call("pods-write", WRITE, Err, |c| {
            let name = name.trim();
            if name.is_empty() {
                return Err("Name is required".into());
            }
            if name.chars().count() > 60 {
                return Err("Name can be at most 60 characters".into());
            }
            let db = c.platform.db();
            match db.execute("INSERT INTO pods (name) VALUES (?1)", [name]) {
                Ok(_) => Ok(types::Pod { id: db.last_insert_rowid() as u32, name: name.to_string() }),
                Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                    Err(format!("A pod named {name:?} already exists"))
                }
                Err(e) => Err(internal("Could not create the pod", e)),
            }
        })
    }
}

impl bookings_read::Host for Ctx {
    fn bookable_hours(&mut self) -> wasmtime::Result<Vec<u8>> {
        self.call("bookings-read", READ, |_| vec![], |_| BOOKABLE_HOURS.collect())
    }

    fn list_for_day(&mut self, date: String) -> wasmtime::Result<Result<Vec<types::Booking>, String>> {
        self.call("bookings-read", READ, Err, |c| {
            valid_date(&date)?;
            let me = c.user.as_ref().map(|u| u.id.clone()).unwrap_or_default();
            let db = c.platform.db();
            let mut stmt = db
                .prepare(
                    "SELECT b.id, b.pod_id, b.date, b.hour, b.subject, u.name, b.booker_id = ?2
                     FROM bookings b JOIN users u ON u.id = b.booker_id
                     WHERE b.date = ?1 ORDER BY b.hour",
                )
                .map_err(|e| internal("The platform could not answer", e))?;
            let rows = stmt
                .query_map(params![date, me], |r| {
                    Ok(types::Booking {
                        id: r.get(0)?,
                        pod_id: r.get(1)?,
                        date: r.get(2)?,
                        hour: r.get(3)?,
                        subject: r.get(4)?,
                        booker_name: r.get(5)?,
                        mine: r.get(6)?,
                    })
                })
                .map_err(|e| internal("The platform could not answer", e))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
    }
}

impl bookings_write::Host for Ctx {
    fn create(
        &mut self,
        pod_id: u32,
        date: String,
        hour: u8,
        subject: String,
    ) -> wasmtime::Result<Result<types::Booking, String>> {
        self.call("bookings-write", WRITE, Err, |c| {
            let user = c.user.clone().ok_or("Sign in to book")?;
            valid_date(&date)?;
            if !BOOKABLE_HOURS.contains(&hour) {
                return Err(format!(
                    "Bookings start between {:02}:00 and {:02}:00",
                    BOOKABLE_HOURS.start(),
                    BOOKABLE_HOURS.end()
                ));
            }
            let subject = subject.trim();
            if subject.is_empty() {
                return Err("Subject is required".into());
            }
            if subject.chars().count() > 80 {
                return Err("Subject can be at most 80 characters".into());
            }
            let db = c.platform.db();
            let pod_exists: bool = db
                .query_row("SELECT EXISTS(SELECT 1 FROM pods WHERE id = ?1)", [pod_id], |r| r.get(0))
                .map_err(|e| internal("The platform could not answer", e))?;
            if !pod_exists {
                return Err("That pod does not exist".into());
            }
            let inserted = db.execute(
                "INSERT INTO bookings (pod_id, date, hour, subject, booker_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![pod_id, date, hour, subject, user.id, crate::db::now()],
            );
            match inserted {
                Ok(_) => Ok(types::Booking {
                    id: db.last_insert_rowid() as u32,
                    pod_id,
                    date,
                    hour,
                    subject: subject.to_string(),
                    booker_name: user.name,
                    mine: true,
                }),
                Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
                    Err("Someone just booked that slot".into())
                }
                Err(e) => Err(internal("Could not book", e)),
            }
        })
    }

    fn cancel_own(&mut self, id: u32) -> wasmtime::Result<Result<(), String>> {
        self.call("bookings-write", WRITE, Err, |c| {
            let user = c.user.clone().ok_or("Sign in to cancel")?;
            let db = c.platform.db();
            let deleted = db
                .execute("DELETE FROM bookings WHERE id = ?1 AND booker_id = ?2", params![id, user.id])
                .map_err(|e| internal("The platform could not answer", e))?;
            if deleted == 0 { Err("You can only cancel your own bookings".into()) } else { Ok(()) }
        })
    }
}

impl identity::Host for Ctx {
    fn current_user(&mut self) -> wasmtime::Result<Option<types::User>> {
        self.call("identity", READ, |_| None, |c| {
            c.user.as_ref().map(|u| types::User { id: u.id.clone(), name: u.name.clone() })
        })
    }
}

impl log::Host for Ctx {
    fn info(&mut self, message: String) -> wasmtime::Result<()> {
        self.call("log", READ, |_| (), |c| println!("[{}] {message}", c.app_id))
    }

    fn warn(&mut self, message: String) -> wasmtime::Result<()> {
        self.call("log", READ, |_| (), |c| eprintln!("[{}] warning: {message}", c.app_id))
    }
}
