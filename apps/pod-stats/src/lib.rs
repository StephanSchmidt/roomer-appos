//! Pod stats: a widget next to the booking grid showing how busy each pod is.

use appos_sdk::{App, Request, Response, bookings_read, fragment, param, pods_read};
use maud::html;

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

        let body = html! {
            div.widget {
                h2 { "How busy is it?" }
                p.big-number { (busy) "%" span.muted { " booked" } }
                ul.pod-stats {
                    @for pod in &pods {
                        @let n = bookings.iter().filter(|b| b.pod_id == pod.id).count();
                        li {
                            div.pod-stat-head { span { (pod.name) } span.muted { (n) "/" (hours) } }
                            progress value=(n) max=(hours) {}
                        }
                    }
                }
            }
        };
        fragment(body.into_string())
    }
}

appos_sdk::export!(PodStats with_types_in appos_sdk::bindings);
