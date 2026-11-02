//! The sandbox. Apps are WebAssembly components run by wasmtime. Each app
//! gets a linker that contains only the platform interfaces its manifest
//! grants, plus a WASI context with no files, no network and no environment.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable, types::ComponentItem};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::db::User;
use crate::manifest::Manifest;
use crate::platform::Platform;

// Platform calls may trap, which is how budgets stop a runaway request.
wasmtime::component::bindgen!({ world: "app", path: "kit/wit", imports: { default: trappable } });

pub use appos::platform::types::{BodyKind, Request, Response};

/// Instructions an app may execute per request. Plenty for rendering a page,
/// and a runaway loop stops instead of hanging the site.
const FUEL_PER_REQUEST: u64 = 2_000_000_000;
/// Memory an app instance may use.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Requests one app may run at the same time. A flood of requests to one
/// app gets 503s for that app instead of taking the server's threads.
const MAX_CONCURRENT_PER_APP: usize = 8;

const PLATFORM_PREFIX: &str = "appos:platform/";

/// WASI interfaces apps may import. They are backed by an empty context, so
/// e.g. stdout goes nowhere and the environment is empty. Network and
/// filesystem interfaces are not on this list and can't be granted.
const ALLOWED_WASI: &[&str] = &[
    "wasi:io/",
    "wasi:clocks/",
    "wasi:random/",
    "wasi:cli/stdout",
    "wasi:cli/stderr",
    "wasi:cli/stdin",
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/terminal-",
];

/// Per-request state of one app instance.
pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    pub platform: Arc<Platform>,
    pub app_id: String,
    pub app_version: String,
    pub user: Option<User>,
    /// I/O units used by this request, and how many it may use.
    pub io_used: u32,
    pub io_limit: u32,
    /// Time this request has spent inside platform calls.
    pub platform_time: std::time::Duration,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

/// A loaded, ready-to-run app version.
pub struct LoadedApp {
    pub manifest: Manifest,
    pre: AppPre<Ctx>,
    /// Limits how many requests this app runs at once.
    pub concurrency: Arc<tokio::sync::Semaphore>,
}

pub fn engine() -> anyhow::Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true);
    Ok(Engine::new(&config)?)
}

/// Compiles a component and lists the interfaces it imports, without versions
/// (e.g. "appos:platform/pods-read", "wasi:cli/stdout").
pub fn compile(engine: &Engine, wasm: &[u8]) -> anyhow::Result<(Component, Vec<String>)> {
    let component = Component::new(engine, wasm).map_err(anyhow::Error::from).context("not a valid WebAssembly component")?;
    let mut imports: Vec<String> = component
        .component_type()
        .imports(engine)
        .filter(|(_, item)| matches!(item.ty, ComponentItem::ComponentInstance(_)))
        .map(|(name, _)| name.split('@').next().unwrap_or(name).to_string())
        .collect();
    imports.sort();
    Ok((component, imports))
}

/// The platform capabilities a component actually uses.
pub fn used_capabilities(imports: &[String]) -> Vec<String> {
    imports
        .iter()
        .filter_map(|i| i.strip_prefix(PLATFORM_PREFIX))
        .filter(|c| *c != "types")
        .map(str::to_string)
        .collect()
}

/// Checks a component's imports against its manifest. Returns why it is
/// refused, if it is.
pub fn check_imports(imports: &[String], manifest: &Manifest) -> Result<(), String> {
    let mut problems = Vec::new();
    for import in imports {
        if let Some(cap) = import.strip_prefix(PLATFORM_PREFIX) {
            if cap != "types" && !manifest.capabilities.grant.iter().any(|g| g == cap) {
                problems.push(format!("uses {cap}, which its manifest does not grant"));
            }
        } else if !ALLOWED_WASI.iter().any(|w| import.starts_with(w)) {
            problems.push(format!("imports {import}, which apps can never use"));
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Builds an app whose linker has only its granted interfaces. If the
/// component imports anything else, instantiation fails here.
pub fn load(engine: &Engine, component: &Component, manifest: Manifest) -> anyhow::Result<LoadedApp> {
    let mut linker: Linker<Ctx> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    appos::platform::types::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?;
    for cap in &manifest.capabilities.grant {
        match cap.as_str() {
            "pods-read" => appos::platform::pods_read::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            "pods-write" => appos::platform::pods_write::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            "bookings-read" => appos::platform::bookings_read::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            "bookings-write" => appos::platform::bookings_write::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            "identity" => appos::platform::identity::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            "log" => appos::platform::log::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c)?,
            other => return Err(anyhow!("unknown capability {other}")),
        }
    }
    let instance_pre = linker.instantiate_pre(component)?;
    let pre = AppPre::new(instance_pre)?;
    Ok(LoadedApp { manifest, pre, concurrency: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PER_APP)) })
}

/// What one request cost, whether it succeeded or not.
pub struct Usage {
    pub io_units: u32,
    pub platform_time: std::time::Duration,
    /// Instructions the app executed.
    pub fuel: u64,
}

/// The CPU budget of a request, for the usage screens.
pub const FUEL_LIMIT: u64 = FUEL_PER_REQUEST;

/// Runs one request in a fresh instance. Nothing survives between requests.
pub fn call(app: &LoadedApp, platform: Arc<Platform>, user: Option<User>, req: Request) -> (anyhow::Result<Response>, Usage) {
    let ctx = Ctx {
        wasi: WasiCtx::builder().build(),
        table: ResourceTable::new(),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        platform,
        app_id: app.manifest.app.id.clone(),
        app_version: app.manifest.app.version.clone(),
        user,
        io_used: 0,
        io_limit: app.manifest.limits.io_per_request,
        platform_time: std::time::Duration::ZERO,
    };
    let mut store = Store::new(app.pre.engine(), ctx);
    store.limiter(|c| &mut c.limits);
    let result = handle(app, &mut store, &req);
    let usage = Usage {
        io_units: store.data().io_used,
        platform_time: store.data().platform_time,
        fuel: FUEL_PER_REQUEST.saturating_sub(store.get_fuel().unwrap_or(0)),
    };
    (result, usage)
}

fn handle(app: &LoadedApp, store: &mut Store<Ctx>, req: &Request) -> anyhow::Result<Response> {
    store.set_fuel(FUEL_PER_REQUEST)?;
    let instance = app.pre.instantiate(&mut *store)?;
    Ok(instance.appos_platform_handler().call_handle(&mut *store, req)?)
}
