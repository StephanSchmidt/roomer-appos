//! Shell-owned HTML: the page frame, sign-in, and the frames around apps.

use std::sync::RwLock;

use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::db::User;
use crate::manifest::Manifest;

/// Top bar links asked for by live apps (label, path), in app id order.
static NAV: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

pub fn set_nav(links: Vec<(String, String)>) {
    *NAV.write().unwrap_or_else(|e| e.into_inner()) = links;
}

/// Output of one app in one slot.
pub enum Slot {
    Ok { manifest: Manifest, body: String },
    Down { manifest: Manifest },
}

pub fn layout(title: &str, user: Option<&User>, path: &str, main: Markup, aside: Vec<Slot>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Roomer" }
                meta name="htmx-config" content=r#"{"includeIndicatorStyles":false,"allowEval":false}"#;
                link rel="stylesheet" href="/assets/app.css";
                script src="/assets/htmx.min.js" defer {}
            }
            body {
                header.topbar {
                    a.brand href="/" {
                        (logo())
                        "Roomer"
                        span.brand-os { "on AppOS" }
                    }
                    @if let Some(user) = user {
                        nav {
                            @for (label, href) in NAV.read().unwrap_or_else(|e| e.into_inner()).iter() {
                                (nav_link(href, label, path))
                            }
                            @if user.is_admin {
                                (nav_link("/admin/apps", "App versions", path))
                                (nav_link("/admin/usage", "Usage", path))
                            }
                        }
                        div.user {
                            span.user-text {
                                span.user-name { (user.name) }
                                a.switch href="/_shell/login" { "Switch user" }
                            }
                            span.avatar title=(format!("{}, {}", user.name, user.team)) { (initials(&user.name)) }
                        }
                    }
                }
                div class=(if aside.is_empty() { "layout" } else { "layout with-aside" }) {
                    main { (main) }
                    @if !aside.is_empty() {
                        aside {
                            @for slot in &aside { (frame(slot)) }
                        }
                    }
                }
            }
        }
    }
}

/// The Roomer mark: a pod's arched doorway.
fn logo() -> Markup {
    html! {
        svg.logo width="30" height="30" viewBox="0 0 24 24" aria-hidden="true" {
            rect width="24" height="24" rx="12" fill="#000000" {}
            path d="M8 18V10.5a4 4 0 0 1 8 0V18" stroke="#FFE45C" stroke-width="2.2" fill="none" stroke-linecap="round" {}
        }
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

fn nav_link(href: &str, label: &str, path: &str) -> Markup {
    let current = if href == "/" { path == "/" } else { path.starts_with(href) };
    html! { a href=(href) class=[current.then_some("current")] { (label) } }
}

/// The frame around an app's output, tagged with the version serving it.
pub fn frame(slot: &Slot) -> Markup {
    match slot {
        Slot::Ok { manifest, body } => html! {
            section.app-frame data-app=(manifest.app.id) {
                (PreEscaped(body))
                (tag(manifest))
            }
        },
        Slot::Down { manifest } => html! {
            section.app-frame.app-down data-app=(manifest.app.id) {
                strong { (manifest.app.name) " is unavailable right now" }
                p { "The rest of the site keeps working. The platform team can see this on the App versions screen." }
                (tag(manifest))
            }
        },
    }
}

fn tag(manifest: &Manifest) -> Markup {
    html! {
        footer.app-tag title=(format!("Served by {} {}", manifest.app.id, manifest.app.version)) {
            span.dot {} (manifest.app.id) " · v" (manifest.app.version)
        }
    }
}

pub fn login(users: &[User]) -> Markup {
    let body = html! {
        header.hero {
            h1 { "Who are you?" }
            p { "Demo sign-in. Pick a person to see Roomer the way they do." }
        }
        form.user-picker method="post" action="/_shell/login" {
            @for u in users {
                button.user-card type="submit" name="user" value=(u.id) {
                    span.avatar.large { (initials(&u.name)) }
                    span.user-card-name { (u.name) }
                    span.user-card-team { (u.team) }
                    @if u.is_admin { span.user-card-role { "Manages apps" } } @else { span.user-card-role { "Books pods" } }
                }
            }
        }
    };
    layout("Sign in", None, "/_shell/login", body, vec![])
}

pub fn message(title: &str, text: &str, user: Option<&User>) -> Markup {
    let body = html! {
        header.hero { h1 { (title) } }
        div.card { p { (text) } }
    };
    layout(title, user, "", body, vec![])
}
