//! HTTP: shell routes, the deploy API, and dispatch of everything else to
//! the app that owns the path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use serde::Deserialize;

use crate::db::{self, User};
use crate::manifest::WIDGET_SLOTS;
use crate::platform::{BUDGET_EXCEEDED, Platform};
use crate::runtime::{self, BodyKind, LoadedApp};
use crate::views::{self, Slot};
use crate::{admin, registry, usage};

const HTMX: &str = include_str!("../assets/htmx.min.js");
const CSS: &str = include_str!("../assets/app.css");
const FONT: &[u8] = include_bytes!("../assets/onest.woff2");

/// Apps cannot run scripts or load anything from elsewhere. The shell's own
/// files are the only scripts and styles on the page.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   form-action 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'";

type AppState = State<Arc<Platform>>;

pub fn router(platform: Arc<Platform>) -> Router {
    Router::new()
        .route("/assets/htmx.min.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], HTMX) }))
        .route("/assets/app.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], CSS) }))
        .route("/assets/onest.woff2", get(|| async { ([(header::CONTENT_TYPE, "font/woff2")], FONT) }))
        .route("/_shell/login", get(login_page).post(login))
        .route("/admin/apps", get(admin_page))
        .route("/admin/apps/{app}/{version}/{action}", post(admin_action))
        .route("/admin/seen", post(admin_seen))
        .route("/admin/usage", get(usage_page))
        .route("/admin/usage/{app}", get(app_usage_page))
        .route("/_platform/deploy", post(api_deploy).layer(DefaultBodyLimit::max(32 * 1024 * 1024)))
        .fallback(dispatch)
        .layer(axum::middleware::map_response(security_headers))
        .with_state(platform)
}

async fn security_headers(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    resp
}

fn current_user(p: &Platform, headers: &HeaderMap) -> Option<User> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    let id = cookies.split(';').find_map(|c| c.trim().strip_prefix("appos_user="))?;
    db::user(&p.db(), id).ok().flatten()
}

fn pairs(s: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(s.as_bytes()).into_owned().collect()
}

// ---- sign-in ---------------------------------------------------------------

async fn login_page(State(p): AppState) -> Html<String> {
    let users = db::users(&p.db()).unwrap_or_default();
    Html(views::login(&users))
}

#[derive(Deserialize)]
struct LoginForm {
    user: String,
}

async fn login(State(p): AppState, axum::Form(form): axum::Form<LoginForm>) -> Response {
    match db::user(&p.db(), &form.user) {
        Ok(Some(u)) => {
            let cookie = format!("appos_user={}; Path=/; HttpOnly; SameSite=Lax", u.id);
            let target = if u.is_admin { "/admin/apps" } else { "/" };
            ([(header::SET_COOKIE, cookie)], Redirect::to(target)).into_response()
        }
        _ => Redirect::to("/_shell/login").into_response(),
    }
}

// ---- admin -----------------------------------------------------------------

#[derive(Deserialize)]
struct AdminQuery {
    error: Option<String>,
    /// "activity" or "access"; the apps overview otherwise.
    view: Option<String>,
}

async fn admin_page(State(p): AppState, headers: HeaderMap, Query(q): Query<AdminQuery>) -> Response {
    let Some(user) = current_user(&p, &headers) else { return Redirect::to("/_shell/login").into_response() };
    if !user.is_admin {
        let page = views::message("Platform team only", "App versions are managed by the platform team.", Some(&user));
        return (StatusCode::FORBIDDEN, Html(page)).into_response();
    }
    let page = admin::page(&p.db(), &p.policy, &user, q.view.as_deref(), q.error.as_deref());
    Html(page).into_response()
}

/// Marks every problem so far as seen, which clears "Needs you".
async fn admin_seen(State(p): AppState, headers: HeaderMap) -> Response {
    match current_user(&p, &headers) {
        Some(user) if user.is_admin => {
            admin::mark_seen(&p.db());
            Redirect::to("/admin/apps").into_response()
        }
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => Redirect::to("/_shell/login").into_response(),
    }
}

async fn usage_page(State(p): AppState, headers: HeaderMap) -> Response {
    let user = match admin_user(&p, &headers) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    Html(usage::dashboard(&p, &user)).into_response()
}

async fn app_usage_page(State(p): AppState, headers: HeaderMap, Path(app): Path<String>) -> Response {
    let user = match admin_user(&p, &headers) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    match usage::app_page(&p, &user, &app) {
        Some(page) => Html(page).into_response(),
        None => {
            let page = views::message("Not found", "No app with that id has run yet.", Some(&user));
            (StatusCode::NOT_FOUND, Html(page)).into_response()
        }
    }
}

/// The signed-in user if they are on the platform team, else the response to send.
fn admin_user(p: &Platform, headers: &HeaderMap) -> Result<User, Box<Response>> {
    let Some(user) = current_user(p, headers) else { return Err(Box::new(Redirect::to("/_shell/login").into_response())) };
    if !user.is_admin {
        let page = views::message("Platform team only", "Usage is visible to the platform team.", Some(&user));
        return Err(Box::new((StatusCode::FORBIDDEN, Html(page)).into_response()));
    }
    Ok(user)
}

async fn admin_action(
    State(p): AppState,
    headers: HeaderMap,
    Path((app, version, action)): Path<(String, String, String)>,
) -> Response {
    let Some(user) = current_user(&p, &headers) else { return Redirect::to("/_shell/login").into_response() };
    if !user.is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = user.name.clone();
    let result = tokio::task::spawn_blocking(move || match action.as_str() {
        "activate" => registry::activate(&p, &app, &version, &actor, "rolled-back"),
        _ => Err("unknown action".into()),
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match result {
        Ok(()) => Redirect::to("/admin/apps").into_response(),
        Err(e) => {
            let q: String = form_urlencoded::Serializer::new(String::new()).append_pair("error", &e).finish();
            Redirect::to(&format!("/admin/apps?{q}")).into_response()
        }
    }
}

// ---- deploy API (used by the CLI) -------------------------------------------

#[derive(Deserialize)]
struct DeployRequest {
    manifest: String,
    wasm_base64: String,
    deployed_by: String,
}

fn authorized(p: &Platform, headers: &HeaderMap) -> bool {
    headers.get("x-appos-token").and_then(|v| v.to_str().ok()) == Some(p.deploy_token.as_str())
}

async fn api_deploy(State(p): AppState, headers: HeaderMap, Json(req): Json<DeployRequest>) -> Response {
    if !authorized(&p, &headers) {
        return (StatusCode::UNAUTHORIZED, "bad deploy token").into_response();
    }
    let Ok(wasm) = base64::engine::general_purpose::STANDARD.decode(&req.wasm_base64) else {
        return (StatusCode::BAD_REQUEST, "wasm is not valid base64").into_response();
    };
    let outcome = tokio::task::spawn_blocking(move || registry::deploy(&p, &req.manifest, &wasm, &req.deployed_by)).await;
    match outcome {
        Ok(o) => Json(o).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---- app dispatch ----------------------------------------------------------

async fn run(p: &Arc<Platform>, app: &Arc<LoadedApp>, user: &User, req: runtime::Request) -> Result<runtime::Response, String> {
    let m = &app.manifest.app;
    let slot = req.slot.clone();
    let Ok(permit) = app.concurrency.clone().try_acquire_owned() else {
        eprintln!("[{}] too many concurrent requests", m.id);
        usage::record(&p.db(), &m.id, &m.version, &slot, "busy", None, Duration::ZERO);
        return Err("too many concurrent requests".into());
    };
    let (p2, app2, user2) = (p.clone(), app.clone(), user.clone());
    let start = Instant::now();
    let (result, cost) = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        runtime::call(&app2, p2, Some(user2), req)
    })
    .await
    {
        Ok((r, cost)) => (r.map_err(|e| format!("{e:#}")), Some(cost)),
        Err(e) => (Err(e.to_string()), None),
    };
    let elapsed = start.elapsed();

    let Err(e) = &result else {
        usage::record(&p.db(), &m.id, &m.version, &slot, "ok", cost.as_ref(), elapsed);
        return result;
    };
    eprintln!("[{}] request failed: {e}", m.id);
    let (kind, message) = if let Some(line) = e.find(BUDGET_EXCEEDED).map(|i| e[i..].lines().next().unwrap_or_default()) {
        ("over-budget", format!("Request stopped: {}", line.trim()))
    } else if e.contains("fuel") {
        ("over-budget", format!("Request stopped: {BUDGET_EXCEEDED}: CPU limit reached"))
    } else {
        ("crashed", format!("Request failed and was contained: {}", e.lines().next().unwrap_or("error")))
    };
    {
        let db = p.db();
        usage::record(&db, &m.id, &m.version, &slot, kind, cost.as_ref(), elapsed);
        db::event(&db, &m.id, &m.version, kind, &message, "appos");
    }
    let (p2, id, version) = (p.clone(), m.id.clone(), m.version.clone());
    let _ = tokio::task::spawn_blocking(move || registry::note_failure(&p2, &id, &version)).await;
    result
}

async fn dispatch(State(p): AppState, req: Request) -> Response {
    let Some(user) = current_user(&p, req.headers()) else {
        return Redirect::to("/_shell/login").into_response();
    };
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let query = pairs(req.uri().query().unwrap_or(""));
    let body = to_bytes(req.into_body(), 64 * 1024).await.unwrap_or_default();
    let form = if method == "GET" { vec![] } else { pairs(std::str::from_utf8(&body).unwrap_or("")) };

    let app = {
        let apps = p.apps.read().unwrap_or_else(|e| e.into_inner());
        apps.values().find(|a| a.manifest.serves(&path)).cloned()
    };
    let Some(app) = app else {
        let page = views::message("Not found", "No app serves this page.", Some(&user));
        return (StatusCode::NOT_FOUND, Html(page)).into_response();
    };

    let clock = chrono::Local::now();
    let today = clock.format("%Y-%m-%d").to_string();
    let now = clock.format("%H:%M").to_string();
    let request = runtime::Request {
        method: method.clone(),
        path: path.clone(),
        slot: "page".into(),
        query: query.clone(),
        form,
        today: today.clone(),
        now: now.clone(),
    };

    let resp = match run(&p, &app, &user, request).await {
        Ok(r) => r,
        Err(_) => {
            let main = views::frame(&Slot::Down { manifest: app.manifest.clone() });
            let page = views::layout(&app.manifest.app.name, Some(&user), &path, main, vec![]);
            return (StatusCode::SERVICE_UNAVAILABLE, Html(page)).into_response();
        }
    };

    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = allowed_headers(&app.manifest.app.id, &resp.headers);

    let body = match resp.kind {
        BodyKind::Fragment => resp.body,
        BodyKind::Page => {
            let mut aside = Vec::new();
            if method == "GET" {
                for (slot, page_path) in WIDGET_SLOTS {
                    if *page_path == path {
                        aside.extend(widgets(&p, &user, slot, &path, &query, &today, &now).await);
                    }
                }
            }
            let main = views::frame(&Slot::Ok { manifest: app.manifest.clone(), body: resp.body });
            let title = resp.title.unwrap_or_else(|| app.manifest.app.name.clone());
            views::layout(&title, Some(&user), &path, main, aside)
        }
    };

    let mut response = (status, Html(body)).into_response();
    response.headers_mut().extend(headers);
    response
}

async fn widgets(
    p: &Arc<Platform>,
    user: &User,
    slot: &str,
    path: &str,
    query: &[(String, String)],
    today: &str,
    now: &str,
) -> Vec<Slot> {
    let mut apps: Vec<Arc<LoadedApp>> = {
        let apps = p.apps.read().unwrap_or_else(|e| e.into_inner());
        apps.values().filter(|a| a.manifest.slots.widgets.iter().any(|w| w == slot)).cloned().collect()
    };
    apps.sort_by(|a, b| a.manifest.app.id.cmp(&b.manifest.app.id));

    let mut out = Vec::new();
    for app in apps {
        let req = runtime::Request {
            method: "GET".into(),
            path: path.into(),
            slot: slot.into(),
            query: query.to_vec(),
            form: vec![],
            today: today.into(),
            now: now.into(),
        };
        let manifest = app.manifest.clone();
        out.push(match run(p, &app, user, req).await {
            Ok(r) => Slot::Ok { manifest, body: r.body },
            Err(_) => Slot::Down { manifest },
        });
    }
    out
}

/// Apps may only set navigation headers, and only to paths on this site.
fn allowed_headers(app_id: &str, headers: &[(String, String)]) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let navigates = name == "location" || name == "hx-redirect" || name == "hx-location";
        let local = value.starts_with('/') && !value.starts_with("//");
        let allowed = (name.starts_with("hx-") || name == "location") && (!navigates || local);
        match (allowed, HeaderName::try_from(name.as_str()), HeaderValue::try_from(value.as_str())) {
            (true, Ok(n), Ok(v)) => {
                out.insert(n, v);
            }
            _ => eprintln!("[{app_id}] dropped header {name}"),
        }
    }
    out
}
