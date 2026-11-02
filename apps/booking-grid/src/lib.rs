//! Booking grid: book a pod in three steps. Pick a pod, pick a time, confirm.

use appos_sdk::{
    App, Booking, Pod, Request, Response, bookings_read, bookings_write, encode, hx_redirect, identity, page, param,
    pods_read, redirect,
};
use chrono::{Datelike, NaiveDate, Weekday};
use maud::{Markup, html};

struct BookingGrid;

impl App for BookingGrid {
    fn handle(req: Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => view(&req),
            ("POST", "/bookings") => create(&req),
            ("DELETE", path) => match path.strip_prefix("/bookings/").and_then(|id| id.parse().ok()) {
                Some(id) => cancel(&req, id),
                None => appos_sdk::not_found(),
            },
            _ => appos_sdk::not_found(),
        }
    }
}

/// Days shown in the day picker.
const DAYS: usize = 5;

/// What one hour of one pod looks like to the signed-in user.
#[derive(Clone, Copy)]
enum State<'a> {
    Past,
    Free,
    Taken(&'a Booking),
    Mine(&'a Booking),
}

/// What the summary card shows.
enum Pick<'a> {
    /// Free hours, `from` up to `to`.
    Range(u8, u8),
    /// Hours you just booked.
    Booked(u8, u8),
    /// One of your bookings, to cancel.
    Yours(&'a Booking),
    Nothing,
}

fn today(req: &Request) -> NaiveDate {
    NaiveDate::parse_from_str(&req.today, "%Y-%m-%d").unwrap_or_default()
}

/// The requested date, or today.
fn date_of(req: &Request, pairs: &[(String, String)]) -> NaiveDate {
    param(pairs, "date").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).unwrap_or(today(req))
}

fn now_hour(req: &Request) -> u8 {
    req.now.split(':').next().and_then(|h| h.parse().ok()).unwrap_or(0)
}

fn ymd(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn hhmm(hour: u8) -> String {
    format!("{hour:02}:00")
}

fn hours_text(n: u8) -> String {
    if n == 1 { "1 hour".into() } else { format!("{n} hours") }
}

fn url(date: NaiveDate, pod: u32, extra: &str) -> String {
    format!("/?date={}&pod={pod}{extra}", ymd(date))
}

fn flag(on: bool) -> &'static str {
    if on { "true" } else { "false" }
}

/// The weekdays from `start`, plus `start` itself even on a weekend.
fn day_window(start: NaiveDate) -> Vec<NaiveDate> {
    start
        .iter_days()
        .enumerate()
        .filter(|(i, d)| *i == 0 || !matches!(d.weekday(), Weekday::Sat | Weekday::Sun))
        .map(|(_, d)| d)
        .take(DAYS)
        .collect()
}

struct Day<'a> {
    date: NaiveDate,
    hours: &'a [u8],
    bookings: Vec<Booking>,
    /// Hours before this one are in the past.
    first_open: u8,
}

impl<'a> Day<'a> {
    fn load(date: NaiveDate, hours: &'a [u8], req: &Request) -> Self {
        let bookings = bookings_read::list_for_day(&ymd(date)).unwrap_or_default();
        let first_open = match date.cmp(&today(req)) {
            std::cmp::Ordering::Less => u8::MAX,
            std::cmp::Ordering::Equal => now_hour(req),
            std::cmp::Ordering::Greater => 0,
        };
        Day { date, hours, bookings, first_open }
    }

    fn state(&self, pod: u32, hour: u8) -> State<'_> {
        match self.bookings.iter().find(|b| b.pod_id == pod && b.hour == hour) {
            Some(b) if b.mine => State::Mine(b),
            Some(b) => State::Taken(b),
            None if hour < self.first_open => State::Past,
            None => State::Free,
        }
    }

    fn free(&self, pod: u32, hour: u8) -> bool {
        self.hours.contains(&hour) && matches!(self.state(pod, hour), State::Free)
    }

    /// Every bookable hour of this day is in the past.
    fn closed(&self) -> bool {
        self.hours.last().is_none_or(|h| *h < self.first_open)
    }

    fn free_hours(&self, pod: u32) -> usize {
        self.hours.iter().filter(|h| self.free(pod, **h)).count()
    }

    fn first_free(&self, pod: u32) -> Option<u8> {
        self.hours.iter().copied().find(|h| self.free(pod, *h))
    }

    /// End of the free run that starts at `from`.
    fn free_until(&self, pod: u32, from: u8) -> u8 {
        let mut end = from;
        while self.free(pod, end) {
            end += 1;
        }
        end
    }

    /// A short line on when the pod is free, for the pod cards.
    fn status(&self, pod: u32, is_today: bool) -> (String, bool) {
        let Some(from) = self.first_free(pod) else {
            return (if is_today { "Booked for the rest of today".into() } else { "Fully booked".into() }, false);
        };
        let until = self.free_until(pod, from);
        if is_today && from <= self.first_open {
            (format!("Free now until {}", hhmm(until)), true)
        } else {
            (format!("Free {}–{}", hhmm(from), hhmm(until)), true)
        }
    }
}

fn view(req: &Request) -> Response {
    let pods = pods_read::all();
    let hours = bookings_read::bookable_hours();
    let user = identity::current_user();
    // Once today's last hour has passed, open on the next working day.
    let today_over = hours.last().is_some_and(|h| now_hour(req) > *h);
    if today_over && param(&req.query, "date").is_none() {
        let next = day_window(today(req))[1];
        return redirect(&format!("/?date={}&closed=1", ymd(next)));
    }
    let date = date_of(req, &req.query);
    let is_today = date == today(req);

    let window = {
        let from_today = day_window(today(req));
        if from_today.contains(&date) { from_today } else { day_window(date) }
    };
    let days: Vec<Day> = window.iter().map(|d| Day::load(*d, &hours, req)).collect();
    let Some(day) = days.iter().find(|d| d.date == date) else { return appos_sdk::not_found() };

    // The asked-for pod, else one that is free now, else one with any free hour.
    let first_hour = hours.first().copied().unwrap_or(0);
    let pod: Option<&Pod> = param(&req.query, "pod")
        .and_then(|p| p.parse::<u32>().ok())
        .and_then(|id| pods.iter().find(|p| p.id == id))
        .or_else(|| pods.iter().find(|p| day.free(p.id, day.first_open.max(first_hour))))
        .or_else(|| pods.iter().find(|p| day.first_free(p.id).is_some()))
        .or(pods.first());
    let Some(pod) = pod else {
        let body = html! {
            header.hero { h1 { "Book a pod" } p { "There are no pods yet. Add one on the Pods page." } }
        };
        return page("Book a pod", body.into_string());
    };

    let hour_param = |k| param(&req.query, k).and_then(|h| h.parse::<u8>().ok());
    let booking = param(&req.query, "booking")
        .and_then(|id| id.parse::<u32>().ok())
        .and_then(|id| day.bookings.iter().find(|b| b.id == id && b.mine && b.pod_id == pod.id));
    let all_mine = |f: u8, t: u8| (f..t).all(|h| matches!(day.state(pod.id, h), State::Mine(_)));
    let pick = match (booking, hour_param("from"), hour_param("to")) {
        (Some(b), _, _) => Pick::Yours(b),
        (None, Some(f), Some(t)) if param(&req.query, "done").is_some() && t > f && all_mine(f, t) => Pick::Booked(f, t),
        (None, Some(f), Some(t)) if t > f && (f..t).all(|h| day.free(pod.id, h)) => Pick::Range(f, t),
        (None, Some(f), _) if day.free(pod.id, f) => Pick::Range(f, f + 1),
        _ => day.first_free(pod.id).map_or(Pick::Nothing, |f| Pick::Range(f, f + 1)),
    };

    let free_now = pods.iter().filter(|p| day.free(p.id, day.first_open)).count();
    let intro = match (is_today, free_now) {
        (true, 0) => "Every pod is busy right now. Pick a later hour below.".to_string(),
        (true, 1) => "One pod is free right now. Pick it, pick your hours, done.".to_string(),
        (true, n) => format!("{n} pods are free right now. Pick one, pick your hours, done."),
        (false, _) if param(&req.query, "closed").is_some() => {
            format!("Pods are closed for today. Here's {}: pick a pod, pick your hours, done.", date.format("%A"))
        }
        (false, _) => format!("Planning ahead for {}. Pick a pod, pick your hours, done.", date.format("%A %-d %B")),
    };

    let body = html! {
        header.hero {
            h1 { "Book a pod" }
            p { (intro) }
        }
        div.booking {
            section.step.step-pods aria-labelledby="step-pod" {
                h2 #step-pod { span.step-n { "1" } "Pick a pod" }
                div role="radiogroup" aria-labelledby="step-pod" {
                    @for p in &pods {
                        @let on = p.id == pod.id;
                        @let (status, free) = day.status(p.id, is_today);
                        a.pod-option.on[on] href=(url(date, p.id, "")) role="radio" aria-checked=(flag(on)) {
                            span.radio {}
                            span.pod-text {
                                span.pod-name { (p.name) }
                                span.pod-meta { (day.free_hours(p.id)) " of " (hours.len()) " hours free" }
                                span.pod-status.free[free] { (status) }
                            }
                        }
                    }
                }
            }

            section.step.step-time aria-labelledby="step-time" {
                h2 #step-time { span.step-n { "2" } "Pick a time" }
                div.days role="group" aria-label="Day" {
                    @for d in &days {
                        @let on = d.date == date;
                        @let free = d.free_hours(pod.id);
                        a.day.on[on] href=(url(d.date, pod.id, "")) aria-current=[on.then_some("date")] {
                            span.day-wk { (d.date.format("%a")) }
                            span.day-n { (d.date.day()) }
                            span.day-free.none[free == 0] {
                                @if free > 0 { (free) " h free" } @else if d.closed() { "Closed" } @else { "Full" }
                            }
                        }
                    }
                }
                div.hours-head {
                    h3 { (pod.name) " on " (if is_today { "today".to_string() } else { date.format("%A").to_string() }) }
                    span { "Tap a start, then an end" }
                }
                div.hours {
                    @for h in &hours {
                        (slot(day, pod.id, *h, &pick))
                    }
                }
            }

            (confirm(req, day, pod, &pick, user.as_ref().map(|u| u.name.as_str())))
        }
    };
    page("Book a pod", body.into_string())
}

/// One hour of the slot picker.
fn slot(day: &Day, pod: u32, hour: u8, pick: &Pick) -> Markup {
    let time = hhmm(hour);
    match day.state(pod, hour) {
        State::Past => html! { span.hour.past aria-disabled="true" { span { (time) } } },
        State::Taken(b) => html! {
            span.hour.taken aria-disabled="true" title=(format!("Booked by {}", b.booker_name)) {
                span { (time) } span.hour-note { (b.booker_name) }
            }
        },
        State::Mine(b) => {
            let on = matches!(pick, Pick::Yours(m) if m.id == b.id)
                || matches!(pick, Pick::Booked(f, t) if (*f..*t).contains(&hour));
            let class = if on { "hour mine on" } else { "hour mine" };
            html! {
                a class=(class) href=(url(day.date, pod, &format!("&booking={}", b.id))) title=(b.subject) aria-pressed=(flag(on)) {
                    span { (time) } span.hour-note { "You" }
                }
            }
        }
        State::Free => {
            let range = match pick {
                Pick::Range(f, t) => Some((*f, *t)),
                _ => None,
            };
            let (on, note) = match range {
                Some((f, t)) if (f..t).contains(&hour) => {
                    (true, if hour == f { "Start" } else if hour + 1 == t { "End" } else { "" })
                }
                _ => (false, ""),
            };
            // A start, then an end: extend the pick when every hour in between is free.
            let (from, to) = match range {
                Some((f, _)) if hour > f && (f..=hour).all(|h| day.free(pod, h)) => (f, hour + 1),
                _ => (hour, hour + 1),
            };
            html! {
                a.hour.free.pick[on] href=(url(day.date, pod, &format!("&from={from}&to={to}"))) aria-pressed=(flag(on)) {
                    span { (time) } span.hour-note { (note) }
                }
            }
        }
    }
}

/// Step 3: the black summary card.
fn confirm(req: &Request, day: &Day, pod: &Pod, pick: &Pick, user: Option<&str>) -> Markup {
    let date = day.date;
    let when = |n: u8| format!("{}, {}", date.format("%A %-d %B"), hours_text(n));
    let range = |from: u8, to: u8| {
        html! {
            div.confirm-time {
                span.confirm-range { (hhmm(from)) "–" (hhmm(to)) }
                span { (when(to - from)) }
            }
        }
    };
    let error = param(&req.query, "error");
    html! {
        aside.confirm aria-label="Summary" {
            @match pick {
                Pick::Yours(b) => {
                    h2 { span.step-n { "3" } "Your booking" }
                    (range(b.hour, b.hour + 1))
                    dl {
                        div { dt { "Pod" } dd { (pod.name) } }
                        div { dt { "For" } dd { (b.subject) } }
                    }
                    @if let Some(e) = error { p.confirm-error { (e) } }
                    button.btn-cancel type="button" hx-delete=(format!("/bookings/{}?date={}&pod={}", b.id, ymd(date), pod.id))
                        hx-confirm=(format!("Cancel your {} booking at {}?", pod.name, hhmm(b.hour))) { "Cancel booking" }
                    a.confirm-link href=(url(date, pod.id, "")) { "Book another time" }
                }
                Pick::Booked(from, to) => {
                    h2 { span.step-n.done { "✓" } "Booked" }
                    (range(*from, *to))
                    dl {
                        div { dt { "Pod" } dd { (pod.name) } }
                        @if let Some(name) = user { div { dt { "Booked for" } dd { (name) } } }
                    }
                    p.confirm-note { "It's yours. To cancel, tap one of your hours." }
                    a.btn-book href=(url(date, pod.id, "")) { "Book another time" }
                }
                Pick::Range(from, to) => {
                    h2 { span.step-n { "3" } "Confirm" }
                    (range(*from, *to))
                    dl {
                        div { dt { "Pod" } dd { (pod.name) } }
                        @if let Some(name) = user { div { dt { "Booking as" } dd { (name) } } }
                    }
                    @if let Some(note) = nearby(day, pod, *from, *to) { p.confirm-note { (note) } }
                    @if let Some(e) = error { p.confirm-error { (e) } }
                    form.confirm-form method="post" action="/bookings" {
                        input type="hidden" name="pod_id" value=(pod.id);
                        input type="hidden" name="date" value=(ymd(date));
                        input type="hidden" name="from" value=(from);
                        input type="hidden" name="to" value=(to);
                        label for="subject" { "What for?" }
                        input #subject type="text" name="subject" placeholder="Writing the Q4 plan" maxlength="80" required;
                        button.btn-book type="submit" { "Book " (pod.name) }
                    }
                    span.confirm-foot { "You can cancel it here until it starts." }
                }
                Pick::Nothing => {
                    h2 { span.step-n { "3" } "Confirm" }
                    p.confirm-empty { (pod.name) " has no free hours left on " (date.format("%A")) ". Try another pod or day." }
                }
            }
        }
    }
}

/// Your other bookings next to or on top of the picked hours.
fn nearby(day: &Day, pod: &Pod, from: u8, to: u8) -> Option<String> {
    let mine: Vec<&Booking> = day.bookings.iter().filter(|b| b.mine).collect();
    if let Some(b) = mine.iter().find(|b| b.pod_id != pod.id && (from..to).contains(&b.hour)) {
        return Some(format!("You already have another pod at {}: {}.", hhmm(b.hour), b.subject));
    }
    let in_pod = |h: u8| mine.iter().any(|b| b.pod_id == pod.id && b.hour == h);
    let mut after = to;
    while in_pod(after) {
        after += 1;
    }
    if after > to {
        Some(format!("Your {} booking follows right after, so you keep {} until {}.", hhmm(to), pod.name, hhmm(after)))
    } else if from > 0 && in_pod(from - 1) {
        Some(format!("You're in {} right before this, so you can stay put.", pod.name))
    } else {
        None
    }
}

fn create(req: &Request) -> Response {
    let date = ymd(date_of(req, &req.form));
    let pod_id: u32 = param(&req.form, "pod_id").and_then(|p| p.parse().ok()).unwrap_or(0);
    // `from` and `to` book a range; `hour` alone books one hour.
    let from: u8 = param(&req.form, "from").or(param(&req.form, "hour")).and_then(|h| h.parse().ok()).unwrap_or(0);
    let to: u8 = param(&req.form, "to").and_then(|h| h.parse().ok()).unwrap_or(0).max(from + 1);
    let subject = param(&req.form, "subject").unwrap_or("");
    let back = format!("/?date={date}&pod={pod_id}&from={from}&to={to}");
    for hour in from..to {
        if let Err(e) = bookings_write::create(pod_id, &date, hour, subject) {
            let e = if hour > from { format!("Booked {}–{} only. {e}.", hhmm(from), hhmm(hour)) } else { e };
            return redirect(&format!("{back}&error={}", encode(&e)));
        }
    }
    redirect(&format!("{back}&done=1"))
}

fn cancel(req: &Request, id: u32) -> Response {
    let date = ymd(date_of(req, &req.query));
    let pod = param(&req.query, "pod").unwrap_or("");
    match bookings_write::cancel_own(id) {
        Ok(()) => hx_redirect(&format!("/?date={date}&pod={pod}")),
        Err(e) => hx_redirect(&format!("/?date={date}&pod={pod}&booking={id}&error={}", encode(&e))),
    }
}

appos_sdk::export!(BookingGrid with_types_in appos_sdk::bindings);
