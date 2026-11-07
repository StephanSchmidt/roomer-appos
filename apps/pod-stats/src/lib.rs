//! Pod stats: a widget next to the booking grid showing how busy each pod is.

use appos_sdk::{App, Request, Response, bookings_read, fragment, param, pods_read, render};
use leptos::prelude::*;

struct PodStats;

impl App for PodStats {
    fn handle(req: Request) -> Response {
        if req.slot != "home.aside" {
            return appos_sdk::not_found();
        }
        let date = param(&req.query, "date").unwrap_or(&req.today);
        let pods = pods_read::all();
        let hours = bookings_read::bookable_hours().len();
        let bookings = bookings_read::list_for_day(date).unwrap_or_default();
        let total = pods.len() * hours;
        let busy = if total == 0 { 0 } else { bookings.len() * 100 / total };

        fragment(render(|| {
            view! {
                <div class="widget">
                    <h2>"How busy is it?"</h2>
                    <p class="big-number">{busy}"%"<span class="muted">" booked"</span></p>
                    <ul class="pod-stats">
                        {pods.into_iter().map(|pod| {
                            let n = bookings.iter().filter(|b| b.pod_id == pod.id).count();
                            view! {
                                <li>
                                    <div class="pod-stat-head"><span>{pod.name}</span><span class="muted">{n}"/"{hours}</span></div>
                                    <progress value=n max=hours></progress>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                </div>
            }
        }))
    }
}

appos_sdk::export!(PodStats with_types_in appos_sdk::bindings);
