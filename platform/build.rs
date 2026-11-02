//! Embeds the apps that come with Roomer: every `../apps/*/app.toml` whose
//! component is built (`make apps`). Without built apps, Roomer starts
//! empty. The shell embeds only the built components and their manifests,
//! never the apps' source.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let apps = Path::new("../apps");
    let release = apps.join("target/wasm32-wasip2/release");
    println!("cargo:rerun-if-changed=../apps");
    println!("cargo:rerun-if-env-changed=ROOMER_NO_APPS");
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    if std::env::var_os("ROOMER_NO_APPS").is_none()
        && let Ok(entries) = std::fs::read_dir(apps)
    {
        for e in entries.flatten() {
            let (manifest, cargo) = (e.path().join("app.toml"), e.path().join("Cargo.toml"));
            let Ok(cargo_text) = std::fs::read_to_string(&cargo) else { continue };
            if !manifest.exists() {
                continue;
            }
            let Some(name) = package_name(&cargo_text) else { continue };
            let wasm = release.join(format!("{}.wasm", name.replace('-', "_")));
            println!("cargo:rerun-if-changed={}", manifest.display());
            println!("cargo:rerun-if-changed={}", wasm.display());
            if wasm.exists() {
                found.push((manifest.canonicalize().unwrap(), wasm.canonicalize().unwrap()));
            } else {
                println!("cargo:warning=not embedding {}: {} is not built (run `make apps`)", e.path().display(), wasm.display());
            }
        }
    }
    found.sort();
    let mut code = String::from("/// Apps built into the binary: (app.toml, component).\npub const APPS: &[(&str, &[u8])] = &[\n");
    for (m, w) in &found {
        let _ = writeln!(code, "    (include_str!({:?}), include_bytes!({:?})),", m, w);
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    std::fs::write(out, code).unwrap();
}

/// `name = "..."` in the `[package]` table.
fn package_name(cargo: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(rest) = line.strip_prefix("name") {
            return Some(rest.trim_start().strip_prefix('=')?.trim().trim_matches('"').to_string());
        }
    }
    None
}
