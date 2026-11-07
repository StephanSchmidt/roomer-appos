# Roomer platform API

This is everything an app needs to know about the Roomer platform. You don't
need, and won't get, the shell's source code. Build against this document,
the contract in `wit/appos.wit` and the SDK in `sdk/`.

Roomer is a pod-booking site. The site itself is a shell run by the platform
team. Your app is a WebAssembly component that the shell runs in a sandbox.
It serves pages or widgets, and it can only reach the platform through the
functions below, and only the ones its manifest asks for.

## How a request reaches your app

The shell owns the page: layout, navigation, `<head>`, scripts, styles and
sign-in. Your app gets one call per request, `handler.handle(request)`, in
a fresh instance. Nothing survives between requests: keep state in the
platform, not in your app.

Every request comes from a signed-in user. There are no anonymous requests.

### `request`

| Field | Meaning |
|-------|---------|
| `method` | `GET`, `POST`, `DELETE`, … as the browser sent it |
| `path` | the URL path, e.g. `/quick` or `/bookings/12` |
| `slot` | `page` for a page or an action on one of your routes; otherwise the widget slot you are rendering, e.g. `home.aside` |
| `query` | the URL's query parameters, in order, as text |
| `form` | the form fields of a `POST`, in order, as text; empty for `GET` |
| `today` | today's date on the site, `YYYY-MM-DD` |
| `now` | the time on the site, `HH:MM` |

Query and form values are whatever the browser sent. Check them: a field
your form fills with a number can hold any text.

### `response`

| Field | Meaning |
|-------|---------|
| `status` | the HTTP status |
| `kind` | `page`: the shell wraps `body` in the site layout. `fragment`: `body` is sent as it is (htmx partials, widgets) |
| `title` | the page title, for `page` responses. You can't write `<head>` yourself |
| `body` | HTML |
| `headers` | only `location` and `hx-*` headers are passed on, and a `location` (or `hx-redirect`, `hx-location`) only to a path on this site, like `/quick?done=1`. Everything else is dropped |

The SDK has helpers: `render(|| view! { … })` turns a Leptos view into
HTML, then `page(title, body)`, `fragment(body)` and `not_found()`.

### What your HTML may do

- **No scripts.** The site's Content Security Policy blocks inline scripts,
  event handlers (`onclick=`) and scripts from anywhere else. Styles come
  from the site's stylesheet; use its classes.
- **htmx is there.** The shell loads htmx, so `hx-get`, `hx-post`,
  `hx-delete`, `hx-target`, `hx-swap` and the other `hx-*` attributes work.
  Point them only at your own routes.
- **Never at the shell's routes.** `/admin`, `/_shell`, `/_platform` and
  `/assets` belong to the shell. Markup that sends requests there on its
  own, or that requests anything the user didn't ask for, fails review.
- **Escape what you show.** Booking subjects, pod names and user names are
  typed in by people. Show them escaped: Leptos's `view!` does, as long as
  you don't put them in `inner_html`.

## The manifest: `app.toml`

```toml
[app]
id = "pod-stats"                 # lowercase letters, digits and dashes
name = "Pod stats"
version = "0.1.0"                # every deploy needs a new version
author = "agent:claude (asked by Alice, Marketing)"
description = "Widget next to the booking grid: how busy each pod is."

[slots]
routes = ["/stats"]              # pages you serve: "/stats" and "/stats/..."
widgets = ["home.aside"]         # widget slots on the shell's pages
nav = "Stats"                    # a top-bar link to your first route (1–24 characters)

[capabilities]
grant = ["pods-read", "bookings-read"]
```

An app needs at least one route or one widget. A route belongs to one app;
`/` matches only the home page. The only widget slot so far is `home.aside`,
next to the booking grid on `/`.

The manifest is the grant: your app gets exactly the capabilities it lists,
and nothing else. A component that uses a capability its manifest doesn't
list is refused when you deploy it.

## Capabilities and functions

Each capability is one interface in `wit/appos.wit`. Call them through the
SDK: `appos_sdk::pods_read::all()` and so on. Errors come back as text
written for people: you may show them to the user as they are.

### `pods-read`: read the list of pods

`all() -> list<pod>`
: Every pod, ordered by id. At most 200. A pod has an `id` and a `name`
  (1–60 characters, unique).

### `pods-write`: create pods

`create(name) -> result<pod, string>`
: Creates a pod. The name is trimmed. Errors: `Name is required`,
  `Name can be at most 60 characters`, `A pod named "…" already exists`.
  Only platform team members may create pods: for everyone else the call
  is denied with `<Name> is not allowed to create pods`, whatever your
  manifest says.

### `bookings-read`: read bookings

`bookable-hours() -> list<u8>`
: The hours a booking can start at, in order: 8 to 16, so the last
  booking ends at 17:00. The platform owns this rule; ask it rather than
  hard-coding hours.

`list-for-day(date) -> result<list<booking>, string>`
: Every booking on one day (`YYYY-MM-DD`), ordered by hour, for all pods.
  Error: `"…" is not a date`.

A `booking` has:

| Field | Meaning |
|-------|---------|
| `id` | the booking's id |
| `pod-id` | the pod it is for |
| `date` | `YYYY-MM-DD` |
| `hour` | the hour it starts; a booking is one hour long |
| `subject` | what it's for, 1–80 characters, typed by the person who booked |
| `booker-name` | who booked it: personal data, show it only where people expect it |
| `mine` | true when the signed-in user made it |

There is one booking per pod and hour at most.

### `bookings-write`: book slots and cancel own bookings

`create(pod-id, date, hour, subject) -> result<booking, string>`
: Books one hour for the signed-in user. To book several hours, call it
  once per hour. Errors: `"…" is not a date`,
  `Bookings start between 08:00 and 16:00`, `Subject is required`,
  `Subject can be at most 80 characters`, `That pod does not exist`,
  `Someone just booked that slot`.

`cancel-own(id) -> result<_, string>`
: Cancels a booking the signed-in user made. Error:
  `You can only cancel your own bookings` (also for bookings that don't
  exist).

### `identity`: see who is signed in

`current-user() -> option<user>`
: The signed-in user: an `id` and a `name`. The name is personal data:
  show it to the user, don't put it in URLs or logs.

### `log`: write to the platform log

`info(message)`, `warn(message)`
: A line in the platform's log, under your app's id. Never log personal
  data.

## Budgets

Every request has a budget: a limit on platform calls, on time spent in
them, on CPU and on memory. A request over budget is stopped, and its
widget or page shows "unavailable" while the rest of the site keeps
working. Load what a page needs with a few calls (`list-for-day` returns
the whole day for every pod), not one call per pod or per hour.

## App review

When you deploy a version, the platform first checks it against its
manifest, then reviews it with [appfuzz](https://appfuzz.dev): it runs your
app thousands of times against a simulated platform that keeps every rule
above, with odd but possible data, and looks at what your app does.

| Verdict | What happens |
|---------|--------------|
| pass | the version goes live |
| warn | the version goes live, and the findings are shown to the platform team |
| fail | the version is refused; the live version keeps serving |

Run the same review yourself before you deploy: `make review`.
