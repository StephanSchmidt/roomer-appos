//! The AppOS SDK: bindings to the platform APIs plus small helpers.
//!
//! An app implements [`App`] and registers it with [`export!`]. Pages are
//! Leptos views, rendered to HTML inside the sandbox with [`render`]:
//!
//! ```ignore
//! use leptos::prelude::*;
//!
//! struct MyApp;
//! impl appos_sdk::App for MyApp {
//!     fn handle(req: Request) -> Response {
//!         appos_sdk::page("Hello", appos_sdk::render(|| view! { <p>"Hi, " {req.path}</p> }))
//!     }
//! }
//! appos_sdk::export!(MyApp with_types_in appos_sdk::bindings);
//! ```

pub mod bindings {
    wit_bindgen::generate!({
        world: "app",
        path: "../wit",
        pub_export_macro: true,
        export_macro_name: "export",
        default_bindings_module: "appos_sdk::bindings",
    });
}

pub use bindings::appos::platform::types::{BodyKind, Booking, Pod, Request, Response, User};
pub use bindings::appos::platform::{bookings_read, bookings_write, identity, log, pods_read, pods_write};
pub use bindings::exports::appos::platform::handler::Guest as App;
pub use bindings::export;

/// Renders a Leptos view to HTML. Text is escaped; the view runs once, on
/// the server, so it needs no signals and ships no JavaScript.
pub fn render<V: leptos::prelude::IntoView>(view: impl FnOnce() -> V) -> String {
    use leptos::prelude::{Owner, RenderHtml};
    Owner::new().with(|| view().into_view().to_html())
}

/// First value for `key` in a list of query or form pairs.
pub fn param<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A full page; the shell wraps it in the site layout.
pub fn page(title: &str, body: String) -> Response {
    Response { status: 200, kind: BodyKind::Page, title: Some(title.to_string()), body, headers: vec![] }
}

/// An HTML fragment returned as is (htmx partials and widgets).
pub fn fragment(body: String) -> Response {
    Response { status: 200, kind: BodyKind::Fragment, title: None, body, headers: vec![] }
}

/// A 303 redirect after a form post.
pub fn redirect(location: &str) -> Response {
    Response {
        status: 303,
        kind: BodyKind::Fragment,
        title: None,
        body: String::new(),
        headers: vec![("location".into(), location.into())],
    }
}

/// Tells htmx to navigate to `location`.
pub fn hx_redirect(location: &str) -> Response {
    Response {
        status: 200,
        kind: BodyKind::Fragment,
        title: None,
        body: String::new(),
        headers: vec![("hx-redirect".into(), location.into())],
    }
}

pub fn not_found() -> Response {
    Response {
        status: 404,
        kind: BodyKind::Page,
        title: Some("Not found".into()),
        body: "<p>Not found.</p>".into(),
        headers: vec![],
    }
}

/// Percent-encodes a value for use in a query string.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
