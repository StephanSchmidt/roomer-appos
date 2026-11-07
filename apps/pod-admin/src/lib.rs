//! Pod admin: list pods and add new ones.

use appos_sdk::{App, Request, Response, page, param, pods_read, pods_write, redirect, render};
use leptos::prelude::*;

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
            let mut resp = list(Some(e));
            resp.status = 400;
            resp
        }
    }
}

fn list(error: Option<String>) -> Response {
    let pods = pods_read::all();
    let empty = pods.is_empty();
    page(
        "Pods",
        render(|| {
            view! {
                <header class="page-header">
                    <h1>"Pods"</h1>
                    <p class="subtitle">"The spaces people can book."</p>
                </header>
                <div class="card">
                    <ul class="pod-list">
                        {pods.into_iter().map(|pod| view! {
                            <li><span class="pod-name">{pod.name}</span><span class="muted">"#"{pod.id}</span></li>
                        }).collect_view()}
                        {empty.then(|| view! { <li class="muted">"No pods yet."</li> })}
                    </ul>
                </div>
                <div class="card">
                    <h2>"Add a pod"</h2>
                    {error.map(|e| view! { <p class="error">{e}</p> })}
                    <form class="inline-form" method="post" action="/pods">
                        <input type="text" name="name" placeholder="e.g. Library Nook" required/>
                        <button class="btn primary" type="submit">"Add pod"</button>
                    </form>
                </div>
            }
        }),
    )
}

appos_sdk::export!(PodAdmin with_types_in appos_sdk::bindings);
