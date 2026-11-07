//! Quick book: book a pod by finishing one sentence. Below it, every pod's
//! day as a band, so you can see why the sentence suggests what it does.

use appos_sdk::{
    App, Booking, Pod, Request, Response, bookings_read, bookings_write, encode, page, param, pods_read, redirect, render,
};
use chrono::{Datelike, NaiveDate, Weekday};
use leptos::prelude::*;

struct QuickBook;

impl App for QuickBook {
    fn handle(req: Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/quick") => view(&req),
            ("POST", "/quick/book") => book(&req),
            _ => appos_sdk::not_found(),
        }
    }
}

/// Days you can pick in the sentence.
const DAYS: usize = 5;
/// How long a suggested booking is, if the pod is free that long.
const SUGGESTED_HOURS: u8 = 2;
/// Columns per hour in the floor bands: quarter hours.
const COLS_PER_HOUR: u32 = 4;

fn today(req: &Request) -> NaiveDate {
    NaiveDate::parse_from_str(&req.today, "%Y-%m-%d").unwrap_or_default()
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

/// Minutes since midnight, from the request's HH:MM.
fn now_minutes(req: &Request) -> u32 {
    let mut parts = req.now.split(':').map(|p| p.parse::<u32>().unwrap_or(0));
    parts.next().unwrap_or(0) * 60 + parts.next().unwrap_or(0)
}

/// Today, then the next working days.
fn day_choices(start: NaiveDate) -> Vec<NaiveDate> {
    start
        .iter_days()
        .enumerate()
        .filter(|(i, d)| *i == 0 || !matches!(d.weekday(), Weekday::Sat | Weekday::Sun))
        .map(|(_, d)| d)
        .take(DAYS)
        .collect()
}

/// How the day reads inside the sentence.
fn day_words(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        0 => "today".into(),
        1 => "tomorrow".into(),
        _ => date.format("on %A").to_string(),
    }
}

/// The current selection, as a link back to this page.
#[derive(Clone, Copy)]
struct Sel {
    date: NaiveDate,
    pod: u32,
    from: u8,
    to: u8,
}

impl Sel {
    fn url(self) -> String {
        format!("/quick?date={}&pod={}&from={}&to={}", ymd(self.date), self.pod, self.from, self.to)
    }
}

struct Floor<'a> {
    hours: &'a [u8],
    bookings: Vec<Booking>,
    /// Hours before this one are in the past.
    first_open: u8,
}

impl Floor<'_> {
    fn booking(&self, pod: u32, hour: u8) -> Option<&Booking> {
        self.bookings.iter().find(|b| b.pod_id == pod && b.hour == hour)
    }

    fn free(&self, pod: u32, hour: u8) -> bool {
        self.hours.contains(&hour) && hour >= self.first_open && self.booking(pod, hour).is_none()
    }

    fn first_free(&self, pod: u32) -> Option<u8> {
        self.hours.iter().copied().find(|h| self.free(pod, *h))
    }

    fn free_until(&self, pod: u32, from: u8) -> u8 {
        let mut end = from;
        while self.free(pod, end) {
            end += 1;
        }
        end
    }

    fn status(&self, pod: u32) -> String {
        match self.first_free(pod) {
            None => "Full".into(),
            Some(f) if f <= self.first_open => format!("Free until {}", hhmm(self.free_until(pod, f))),
            Some(f) => format!("Free from {}", hhmm(f)),
        }
    }
}

fn view(req: &Request) -> Response {
    let pods = pods_read::all();
    let hours = bookings_read::bookable_hours();
    let (Some(&open), Some(&last)) = (hours.first(), hours.last()) else { return appos_sdk::not_found() };
    let close = last + 1;
    let now = now_minutes(req);

    // After the last bookable hour, suggest the next working day.
    let days = day_choices(today(req));
    let date = param(&req.query, "date")
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or(if now / 60 > u32::from(last) { days[1] } else { days[0] });
    let is_today = date == today(req);
    let first_open = match date.cmp(&today(req)) {
        std::cmp::Ordering::Less => u8::MAX,
        std::cmp::Ordering::Equal => (now / 60) as u8,
        std::cmp::Ordering::Greater => 0,
    };
    let floor =
        Floor { hours: &hours, bookings: bookings_read::list_for_day(&ymd(date)).unwrap_or_default(), first_open };

    let hour_param = |k| param(&req.query, k).and_then(|h| h.parse::<u8>().ok());
    let done = param(&req.query, "done").is_some();
    let pod: Option<&Pod> = param(&req.query, "pod")
        .and_then(|p| p.parse::<u32>().ok())
        .and_then(|id| pods.iter().find(|p| p.id == id))
        .or_else(|| pods.iter().find(|p| floor.free(p.id, first_open.max(open))))
        .or_else(|| pods.iter().find(|p| floor.first_free(p.id).is_some()))
        .or(pods.first());
    let Some(pod) = pod else {
        let body = render(|| view! { <header class="hero"><h1>"Quick book"</h1><p>"There are no pods yet."</p></header> });
        return page("Quick book", body);
    };

    // Just booked: the hours are now yours. Otherwise a free range, as asked or suggested.
    let mine_range = |f: u8, t: u8| t > f && (f..t).all(|h| floor.booking(pod.id, h).is_some_and(|b| b.mine));
    let range: Option<(u8, u8)> = match (hour_param("from"), hour_param("to")) {
        (Some(f), Some(t)) if done && mine_range(f, t) => Some((f, t)),
        (Some(f), Some(t)) if !done && t > f && t <= floor.free_until(pod.id, f) => Some((f, t)),
        (Some(f), _) if !done && floor.free(pod.id, f) => Some((f, (f + SUGGESTED_HOURS).min(floor.free_until(pod.id, f)))),
        _ => floor.first_free(pod.id).map(|f| (f, (f + SUGGESTED_HOURS).min(floor.free_until(pod.id, f)))),
    };
    let done = done && range.is_some_and(|(f, t)| mine_range(f, t));
    let sel = Sel { date, pod: pod.id, from: range.map_or(open, |r| r.0), to: range.map_or(open + 1, |r| r.1) };

    let free_now = pods.iter().filter(|p| floor.free(p.id, first_open.max(open))).count();
    let lede = if is_today {
        let pods_free = match free_now {
            0 => "Every pod is busy right now.".to_string(),
            1 => "One pod is free right now.".to_string(),
            n => format!("{n} pods are free right now."),
        };
        format!("{}, {}. {pods_free}", date.format("%A %-d %B"), req.now)
    } else {
        let with_time = pods.iter().filter(|p| floor.first_free(p.id).is_some()).count();
        format!("{}. {with_time} of {} pods have free time.", date.format("%A %-d %B"), pods.len())
    };
    let day = day_words(date, today(req));
    let error = param(&req.query, "error").map(|e| e.to_string());
    let next_day = days[(days.iter().position(|d| *d == date).unwrap_or(0) + 1) % DAYS];
    // The part of today's bands that is already over, in quarter hours.
    let past_end = if first_open <= open {
        0
    } else if is_today {
        (now.saturating_sub(u32::from(open) * 60) / 15).min(u32::from(close - open) * COLS_PER_HOUR)
    } else {
        u32::from(close - open) * COLS_PER_HOUR
    };
    let now_line = (is_today && now >= u32::from(open) * 60 && now < u32::from(close) * 60)
        .then(|| format!("now-line at-{}", (now - u32::from(open) * 60) / 15 + 1));

    let sentence = match (range, done) {
        (Some((from, to)), true) => view! {
            <div class="sentence">"You've got the "<u>{pod.name.clone()}</u>" "{day.clone()}" from "<u>{hhmm(from)}</u>" to "<u>{hhmm(to)}</u>"."</div>
            <div class="sentence-actions">
                <a class="btn-pill" href=format!("/quick?date={}", ymd(date))>"Book something else"</a>
                <span>"Cancel it on Book a pod."</span>
            </div>
        }
        .into_any(),
        (Some((from, to)), false) => {
            let pod_options = pods
                .iter()
                .map(|p| {
                    let free = floor.first_free(p.id).is_some();
                    (p.name.clone(), Some(floor.status(p.id)), free.then(|| Sel { pod: p.id, ..sel }.url()), p.id == pod.id)
                })
                .collect();
            let day_options = days
                .iter()
                .map(|d| {
                    let words = day_words(*d, today(req));
                    let label = if words.starts_with("on ") { d.format("%A %-d %B").to_string() } else { words };
                    (label, None, Some(format!("/quick?date={}&pod={}", ymd(*d), pod.id)), *d == date)
                })
                .collect();
            let start_options = hours
                .iter()
                .filter(|h| floor.free(pod.id, **h))
                .map(|h| {
                    let to = (h + SUGGESTED_HOURS).min(floor.free_until(pod.id, *h));
                    (hhmm(*h), None, Some(Sel { from: *h, to, ..sel }.url()), *h == from)
                })
                .collect();
            let end_options = (from + 1..=floor.free_until(pod.id, from))
                .map(|t| (hhmm(t), Some(hours_text(t - from)), Some(Sel { to: t, ..sel }.url()), t == to))
                .collect();
            view! {
                <form class="sentence-form" method="post" action="/quick/book">
                    <input type="hidden" name="date" value=ymd(date)/>
                    <input type="hidden" name="pod_id" value=pod.id/>
                    <input type="hidden" name="from" value=from/>
                    <input type="hidden" name="to" value=to/>
                    <div class="sentence">
                        "Book the "
                        {choice(&pod.name, "Pick a pod", pod_options)}
                        " "
                        {choice(&day, "Pick a day", day_options)}
                        " from "
                        {choice(&hhmm(from), "Start at", start_options)}
                        " to "
                        {choice(&hhmm(to), "End at", end_options)}
                        " for "
                        <label class="sr-only" for="subject">"What for?"</label>
                        <input id="subject" class="sentence-input" type="text" name="subject" placeholder="what?" maxlength="80" required/>
                    </div>
                    {error.map(|e| view! { <p class="sentence-error">{e}</p> })}
                    <div class="sentence-actions">
                        <button class="btn-pill" type="submit">
                            "Book it"
                            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="#FFE45C" stroke-width="2.6"
                                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"></path></svg>
                        </button>
                        <span>{hours_text(to - from)}" in the "{pod.name.clone()}". "{nearby(&floor, pod, from, to)}</span>
                    </div>
                </form>
            }
            .into_any()
        }
        (None, _) => view! {
            <div class="sentence">"The "<u>{pod.name.clone()}</u>" is full "{day.clone()}"."</div>
            <div class="sentence-actions">
                <span>"Pick another pod below, or "<a href=format!("/quick?date={}", ymd(next_day))>"try the next day"</a>"."</span>
            </div>
        }
        .into_any(),
    };

    let body = render(|| {
        view! {
            <section class="sentence-hero bleed-top" aria-label="Book a pod">
                <p class="lede">{lede}</p>
                {sentence}
            </section>

            <section class="floor" aria-labelledby="floor-title">
                <div class="floor-head">
                    <h2 id="floor-title">"The floor "{day.clone()}</h2>
                    <div class="floor-scale" aria-hidden="true">
                        {(open..close).step_by(2).map(|h| view! { <span class=format!("at-{}", col(h, 0, open))>{hhmm(h)}</span> }).collect_view()}
                        <span class="scale-end">{hhmm(close)}</span>
                    </div>
                    <span></span>
                </div>
                {pods.iter().map(|p| {
                    let on = p.id == pod.id;
                    let status = match (on && range.is_some(), done) {
                        (true, true) => "Yours".to_string(),
                        (true, false) => "Your pick".to_string(),
                        (false, _) => floor.status(p.id),
                    };
                    view! {
                        <a class="floor-row" class:on=on href=Sel { pod: p.id, ..sel }.url() aria-current=on.then_some("true")>
                            <span class="floor-pod">{p.name.clone()}</span>
                            <span class="band">
                                {(past_end > 0).then(|| view! { <span class=format!("band-past at-1 span-{past_end}")></span> })}
                                {blocks(&floor, p.id, if on && !done { range } else { None }).into_iter().map(|(from, to, kind, label)| view! {
                                    <span class=format!("block block-{kind} at-{} span-{}", col(from, 0, open), u32::from(to - from) * COLS_PER_HOUR)>{label}</span>
                                }).collect_view()}
                                {now_line.clone().map(|class| view! { <span class=class></span> })}
                            </span>
                            <span class="floor-status" class:free={floor.first_free(p.id).is_some() && !on}>{status}</span>
                        </a>
                    }
                }).collect_view()}
            </section>
        }
    });
    page("Quick book", body)
}

/// The grid column where `hour:minute` starts on a floor band.
fn col(hour: u8, minute: u32, open: u8) -> u32 {
    u32::from(hour.saturating_sub(open)) * COLS_PER_HOUR + minute / 15 + 1
}

/// One underlined word of the sentence that opens a list of choices.
/// Each option is (label, detail, link if it can be picked, selected).
fn choice(word: &str, title: &str, options: Vec<(String, Option<String>, Option<String>, bool)>) -> impl IntoView + use<> {
    let (word, menu, title) = (word.to_string(), title.to_string(), title.to_string());
    view! {
        <details class="choice">
            <summary>{word}</summary>
            <div class="choice-menu" role="menu" aria-label=menu>
                <span class="choice-title">{title}</span>
                {options.into_iter().map(|(label, detail, href, on)| {
                    let detail = detail.map(|d| view! { <span class="choice-detail">{d}</span> });
                    match href {
                        Some(href) => view! {
                            <a class="choice-item" class:on=on role="menuitem" href=href><span>{label}</span>{detail}</a>
                        }
                        .into_any(),
                        None => view! {
                            <span class="choice-item off" role="menuitem" aria-disabled="true"><span>{label}</span>{detail}</span>
                        }
                        .into_any(),
                    }
                }).collect_view()}
            </div>
        </details>
    }
}

/// Bookings on one pod's band, merged into runs: (from, to, kind, label).
fn blocks(floor: &Floor, pod: u32, pick: Option<(u8, u8)>) -> Vec<(u8, u8, &'static str, String)> {
    let mut out: Vec<(u8, u8, &'static str, String)> = Vec::new();
    for &h in floor.hours {
        let item = match (floor.booking(pod, h), pick) {
            (Some(b), _) if b.mine => ("mine", "You".to_string()),
            (Some(b), _) => ("other", b.booker_name.clone()),
            (None, Some((f, t))) if (f..t).contains(&h) => {
                ("pick", if t - f == 1 { hhmm(f) } else { format!("{}–{}", hhmm(f), hhmm(t)) })
            }
            (None, _) => continue,
        };
        match out.last_mut() {
            Some(last) if last.1 == h && last.2 == item.0 && last.3 == item.1 => last.1 = h + 1,
            _ => out.push((h, h + 1, item.0, item.1)),
        }
    }
    out
}

/// A line about your own bookings around the picked hours, if there is one.
fn nearby(floor: &Floor, pod: &Pod, from: u8, to: u8) -> Option<String> {
    let mine = |p: u32, h: u8| floor.booking(p, h).is_some_and(|b| b.mine);
    if let Some(b) = floor.bookings.iter().find(|b| b.mine && b.pod_id != pod.id && (from..to).contains(&b.hour)) {
        return Some(format!("Heads up: you already have another pod at {}.", hhmm(b.hour)));
    }
    let mut until = to;
    while mine(pod.id, until) {
        until += 1;
    }
    (until > to).then(|| format!("Your {} booking is in the same room, so you can stay until {}.", hhmm(to), hhmm(until)))
}

fn book(req: &Request) -> Response {
    let date = param(&req.form, "date").unwrap_or("");
    let pod_id: u32 = param(&req.form, "pod_id").and_then(|p| p.parse().ok()).unwrap_or(0);
    let from: u8 = param(&req.form, "from").and_then(|h| h.parse().ok()).unwrap_or(0);
    let to: u8 = param(&req.form, "to").and_then(|h| h.parse().ok()).unwrap_or(0).max(from + 1);
    let subject = param(&req.form, "subject").unwrap_or("");
    let back = format!("/quick?date={}&pod={pod_id}&from={from}&to={to}", encode(date));
    for hour in from..to {
        if let Err(e) = bookings_write::create(pod_id, date, hour, subject) {
            let e = if hour > from { format!("Booked {}–{} only. {e}.", hhmm(from), hhmm(hour)) } else { e };
            return redirect(&format!("{back}&error={}", encode(&e)));
        }
    }
    redirect(&format!("{back}&done=1"))
}

appos_sdk::export!(QuickBook with_types_in appos_sdk::bindings);
