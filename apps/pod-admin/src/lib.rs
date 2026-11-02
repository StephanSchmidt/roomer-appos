//! Pod admin: list pods and add new ones.

use appos_sdk::{App, Request, Response, page, param, pods_read, pods_write, redirect};
use maud::html;

struct PodAdmin;

impl App for PodAdmin {
    fn handle(req: Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/pods") => list(None),
            ("POST", "/pods") => create(&req),
            _ => appos_sdk::not_found(),
        }
    }
}

fn create(req: &Request) -> Response {
    let name = param(&req.form, "name").unwrap_or("");
    match pods_write::create(name) {
        Ok(_) => redirect("/pods"),
        Err(e) => {
            let mut resp = list(Some(&e));
            resp.status = 400;
            resp
        }
    }
}

fn list(error: Option<&str>) -> Response {
    let pods = pods_read::all();
    let body = html! {
        header.page-header {
            h1 { "Pods" }
            p.subtitle { "The spaces people can book." }
        }
        div.card {
            ul.pod-list {
                @for pod in &pods {
                    li { span.pod-name { (pod.name) } span.muted { "#" (pod.id) } }
                }
                @if pods.is_empty() {
                    li.muted { "No pods yet." }
                }
            }
        }
        div.card {
            h2 { "Add a pod" }
            @if let Some(e) = error {
                p.error { (e) }
            }
            form.inline-form method="post" action="/pods" {
                input type="text" name="name" placeholder="e.g. Library Nook" required;
                button.btn.primary type="submit" { "Add pod" }
            }
        }
    };
    page("Pods", body.into_string())
}

appos_sdk::export!(PodAdmin with_types_in appos_sdk::bindings);
