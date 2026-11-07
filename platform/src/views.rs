//! Shell-owned HTML: the page frame, sign-in, and the frames around apps.
//! Leptos views, rendered to HTML on the server.

use std::sync::RwLock;

use leptos::prelude::*;

use crate::db::User;
use crate::manifest::Manifest;

/// Top bar links asked for by live apps (label, path), in app id order.
static NAV: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

pub fn set_nav(links: Vec<(String, String)>) {
    *NAV.write().unwrap_or_else(|e| e.into_inner()) = links;
}

/// Renders a view to HTML. The shell's pages run once, on the server: no
/// signals, no JavaScript of their own.
pub fn render<V: IntoView>(view: impl FnOnce() -> V) -> String {
    Owner::new().with(|| view().into_view().to_html())
}

/// Output of one app in one slot.
pub enum Slot {
    Ok { manifest: Manifest, body: String },
    Down { manifest: Manifest },
}

/// A whole page: the site's frame around `main`, with app widgets beside it.
pub fn layout(title: &str, user: Option<&User>, path: &str, main: AnyView, aside: Vec<Slot>) -> String {
    // One text node: a browser shows anything inside <title> as text.
    let title = format!("{title} · Roomer");
    let top = user.map(|user| {
        let mut links: Vec<AnyView> =
            NAV.read().unwrap_or_else(|e| e.into_inner()).iter().map(|(label, href)| nav_link(href, label, path)).collect();
        if user.is_admin {
            links.push(nav_link("/admin/apps", "App versions", path));
            links.push(nav_link("/admin/usage", "Usage", path));
        }
        let (name, initials, tip) = (user.name.clone(), initials(&user.name), format!("{}, {}", user.name, user.team));
        view! {
            <nav>{links}</nav>
            <div class="user">
                <span class="user-text">
                    <span class="user-name">{name}</span>
                    <a class="switch" href="/_shell/login">"Switch user"</a>
                </span>
                <span class="avatar" title=tip>{initials}</span>
            </div>
        }
    });
    let layout = if aside.is_empty() { "layout" } else { "layout with-aside" };
    let aside = (!aside.is_empty()).then(|| view! { <aside>{aside.iter().map(frame).collect_view()}</aside> });
    render(|| {
        view! {
            <!DOCTYPE html>
            <html lang="en">
                <head>
                    <meta charset="utf-8"/>
                    <meta name="viewport" content="width=device-width, initial-scale=1"/>
                    <title>{title}</title>
                    <meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false}"#/>
                    <link rel="stylesheet" href="/assets/app.css"/>
                    <script src="/assets/htmx.min.js" defer></script>
                </head>
                <body>
                    <header class="topbar">
                        <a class="brand" href="/">
                            {logo()}
                            "Roomer"
                            <span class="brand-os">"on AppOS"</span>
                        </a>
                        {top}
                    </header>
                    <div class=layout>
                        <main>{main}</main>
                        {aside}
                    </div>
                </body>
            </html>
        }
    })
}

/// The Roomer mark: a pod's arched doorway.
fn logo() -> impl IntoView {
    view! {
        <svg class="logo" width="30" height="30" viewBox="0 0 24 24" aria-hidden="true">
            <rect width="24" height="24" rx="12" fill="#000000"></rect>
            <path d="M8 18V10.5a4 4 0 0 1 8 0V18" stroke="#FFE45C" stroke-width="2.2" fill="none" stroke-linecap="round"></path>
        </svg>
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

fn nav_link(href: &str, label: &str, path: &str) -> AnyView {
    let current = if href == "/" { path == "/" } else { path.starts_with(href) };
    let (href, label) = (href.to_string(), label.to_string());
    if current {
        view! { <a href=href class="current">{label}</a> }.into_any()
    } else {
        view! { <a href=href>{label}</a> }.into_any()
    }
}

/// The frame around an app's output, tagged with the version serving it.
/// The app's HTML goes in as it is: app review checked it, and the site's
/// Content Security Policy keeps scripts out.
pub fn frame(slot: &Slot) -> AnyView {
    match slot {
        Slot::Ok { manifest, body } => view! {
            <section class="app-frame" data-app=manifest.app.id.clone()>
                <div class="app-body" inner_html=body.clone()></div>
                {tag(manifest)}
            </section>
        }
        .into_any(),
        Slot::Down { manifest } => view! {
            <section class="app-frame app-down" data-app=manifest.app.id.clone()>
                <strong>{format!("{} is unavailable right now", manifest.app.name)}</strong>
                <p>"The rest of the site keeps working. The platform team can see this on the App versions screen."</p>
                {tag(manifest)}
            </section>
        }
        .into_any(),
    }
}

fn tag(manifest: &Manifest) -> impl IntoView + use<> {
    let (id, version) = (manifest.app.id.clone(), manifest.app.version.clone());
    let tip = format!("Served by {id} {version}");
    view! {
        <footer class="app-tag" title=tip>
            <span class="dot"></span>{id}" · v"{version}
        </footer>
    }
}

pub fn login(users: &[User]) -> String {
    let cards = users
        .iter()
        .map(|u| {
            let role = if u.is_admin { "Manages apps" } else { "Books pods" };
            view! {
                <button class="user-card" type="submit" name="user" value=u.id.clone()>
                    <span class="avatar large">{initials(&u.name)}</span>
                    <span class="user-card-name">{u.name.clone()}</span>
                    <span class="user-card-team">{u.team.clone()}</span>
                    <span class="user-card-role">{role}</span>
                </button>
            }
        })
        .collect_view();
    let body = view! {
        <header class="hero">
            <h1>"Who are you?"</h1>
            <p>"Demo sign-in. Pick a person to see Roomer the way they do."</p>
        </header>
        <form class="user-picker" method="post" action="/_shell/login">{cards}</form>
    };
    layout("Sign in", None, "/_shell/login", body.into_any(), vec![])
}

pub fn message(title: &str, text: &str, user: Option<&User>) -> String {
    let body = view! {
        <header class="hero"><h1>{title.to_string()}</h1></header>
        <div class="card"><p>{text.to_string()}</p></div>
    };
    layout(title, user, "", body.into_any(), vec![])
}
