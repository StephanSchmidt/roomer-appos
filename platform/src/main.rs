//! `roomer`: the Roomer shell (AppOS architecture) and its deploy tool.
//!
//! - `roomer` (or `roomer serve`) runs the site, with the preinstalled apps.
//! - `roomer deploy --manifest app.toml --wasm app.wasm` ships a built app.
//!   The shell checks it against its manifest, reviews it with appfuzz, and
//!   decides.
//! - `roomer manifest --manifest app.toml --wasm app.wasm` prints the appfuzz
//!   manifest the platform reviews the app with.

mod admin;
mod db;
mod embedded;
mod manifest;
mod platform;
mod policy;
mod registry;
mod review;
mod runtime;
mod usage;
mod views;
mod web;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Context;
use base64::Engine as _;
use clap::{Parser, Subcommand};

use crate::manifest::Manifest;
use crate::platform::Platform;

const DATA_DIR: &str = "roomer-data";
const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

#[derive(Parser)]
#[command(name = "roomer", version, about = "Roomer: a pod-booking site built as an AppOS shell")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the shell (the default).
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        /// How long appfuzz fuzzes each new version before the shell decides.
        #[arg(long, default_value = review::DEFAULT_TIME)]
        review_time: String,
    },
    /// Deploy a built app to a running shell. The shell reviews it first.
    Deploy {
        /// The app's manifest, e.g. apps/booking-grid/app.toml
        #[arg(long)]
        manifest: PathBuf,
        /// The built component, e.g. apps/target/wasm32-wasip2/release/booking_grid.wasm
        #[arg(long)]
        wasm: PathBuf,
        /// Who is deploying
        #[arg(long, default_value = "agent:claude")]
        by: String,
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
    /// Print the appfuzz manifest the platform reviews an app with.
    Manifest {
        #[arg(long)]
        manifest: PathBuf,
        /// The component, as the appfuzz manifest should refer to it
        #[arg(long)]
        wasm: String,
    },
    /// Check that appfuzz is installed and new enough.
    CheckAppfuzz,
    /// Summarize an `appfuzz review` report; exits 0 pass, 1 warn, 2 fail.
    Verdict {
        /// The JSON `appfuzz review` printed
        report: PathBuf,
        /// What to call the app in the summary
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command.unwrap_or(Cmd::Serve { addr: "127.0.0.1:8080".into(), review_time: review::DEFAULT_TIME.into() }) {
        Cmd::Serve { addr, review_time } => serve(&addr, &review_time),
        Cmd::Deploy { manifest, wasm, by, server } => deploy(&manifest, &wasm, &by, &server),
        Cmd::Manifest { manifest, wasm } => {
            let m = read_manifest(&manifest)?.1;
            print!("{}", review::appfuzz_manifest(&m, &wasm));
            Ok(())
        }
        Cmd::Verdict { report, name } => {
            let text = std::fs::read(&report).with_context(|| format!("reading {}", report.display()))?;
            let r = review::parse(&text, &report.parent().unwrap_or(Path::new(".")).display().to_string()).map_err(anyhow::Error::msg)?;
            let name = name.unwrap_or_else(|| report.display().to_string());
            let (color, code) = match r.verdict.as_str() {
                "pass" => ("32", 0),
                "warn" => ("33", 1),
                _ => ("31", 2),
            };
            println!("\x1b[{color}m{name}: {}\x1b[0m, {} runs", r.verdict, r.runs);
            for f in &r.findings {
                println!("  {:<5} {:<24} {}", f.severity, f.kind, f.title);
            }
            std::process::exit(code);
        }
        Cmd::CheckAppfuzz => {
            let version = review::check_appfuzz().map_err(anyhow::Error::msg)?;
            println!("appfuzz {version}");
            Ok(())
        }
    }
}

fn read_manifest(path: &Path) -> anyhow::Result<(String, Manifest)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let m = Manifest::parse(&text).map_err(anyhow::Error::msg)?;
    Ok((text, m))
}

fn token_path() -> PathBuf {
    Path::new(DATA_DIR).join("deploy.token")
}

fn load_or_create_token() -> anyhow::Result<String> {
    if let Ok(t) = std::fs::read_to_string(token_path()) {
        return Ok(t.trim().to_string());
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(token_path(), &token)?;
    Ok(token)
}

fn serve(addr: &str, review_time: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(DATA_DIR)?;
    if let Err(e) = review::check_appfuzz() {
        eprintln!("warning: {e}\n         Until then every deploy is refused: the shell reviews each version before it goes live.");
    }
    let conn = db::open(&format!("{DATA_DIR}/roomer.db"))?;
    let policy = policy::Policy::new(&db::users(&conn)?)?;
    let platform = Arc::new(Platform {
        db: Mutex::new(conn),
        policy,
        engine: runtime::engine()?,
        apps: RwLock::new(HashMap::new()),
        failures: Mutex::new(HashMap::new()),
        deploy_token: load_or_create_token()?,
        review_time: review_time.to_string(),
    });
    registry::install_preinstalled(&platform);
    registry::load_active(&platform);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("Roomer: http://{addr}");
        axum::serve(listener, web::router(platform)).await?;
        Ok(())
    })
}

fn deploy(manifest: &Path, wasm_path: &Path, by: &str, server: &str) -> anyhow::Result<()> {
    let (manifest_text, m) = read_manifest(manifest)?;
    let wasm = std::fs::read(wasm_path).with_context(|| format!("reading {}", wasm_path.display()))?;
    println!("Deploying {} v{} ({} KB); the shell reviews it first ...", m.app.id, m.app.version, wasm.len() / 1024);

    let token = std::fs::read_to_string(token_path()).context("no deploy token; is `roomer` running in this directory?")?;
    let body = serde_json::json!({
        "manifest": manifest_text,
        "wasm_base64": base64::engine::general_purpose::STANDARD.encode(&wasm),
        "deployed_by": by,
    });
    let outcome: serde_json::Value = ureq::post(format!("{server}/_platform/deploy"))
        .header("x-appos-token", token.trim())
        .send_json(&body)
        .with_context(|| format!("is `roomer` running at {server}?"))?
        .body_mut()
        .read_json()?;

    let status = outcome["status"].as_str().unwrap_or("?");
    let message = outcome["message"].as_str().unwrap_or("");
    let (icon, color) = match status {
        "live" => ("✔", "32"),
        "flagged" => ("▲", "33"),
        _ => ("✖", "31"),
    };
    println!("\x1b[{color}m{icon} {} v{}: {message}\x1b[0m", m.app.id, m.app.version);
    if status == "refused" {
        std::process::exit(1);
    }
    Ok(())
}
