# Roomer on AppOS

Your phone runs dozens of apps from strangers, and you don't worry. Your
website runs code from your own colleagues' agents, and you're terrified. Why?

Because the phone doesn't trust the code. It trusts the sandbox, the
permissions, and the app store's review. **AppOS architecture** applies the
same idea to a web app: the site is a shell that runs apps, every app
declares the permissions it needs, the platform enforces them, and every
version is reviewed automatically before it goes live.

Roomer is a small pod-booking site built this way, in Rust. It is the demo
for [appfuzz](https://appfuzz.dev), the app review.

## Two sides

| | Who | What |
|-|-----|------|
| `platform/` | the platform team | the shell: sandbox, platform APIs, sign-in, layout, app versions, review. Ships as one binary, `roomer` |
| `apps/` | marketing and product, through their agents | four apps, each a WebAssembly component with a manifest (`app.toml`) |

The apps never see the shell's code. They build against the **platform
kit** in `apps/kit/`: [HOST-API.md](apps/kit/HOST-API.md) (what every
platform function does), the contract (`wit/appos.wit`) and the SDK. The kit
is a copy of `platform/kit/`; `make check` fails if the copy differs or if
anything in `apps/` refers to `platform/`.

## Try it

You need [appfuzz](https://appfuzz.dev), the `roomer` shell, and Rust with
the `wasm32-wasip2` target.

```sh
curl -fsSL https://appfuzz.dev/install.sh | sh     # appfuzz
curl -fsSL https://raw.githubusercontent.com/StephanSchmidt/roomer-appos/main/install.sh | sh   # the roomer shell
rustup target add wasm32-wasip2
git clone https://github.com/StephanSchmidt/roomer-appos && cd roomer-appos
```

Start the shell in the repo folder (it keeps its data in `./roomer-data`):

```sh
roomer
```

Open http://127.0.0.1:8080 and sign in as **Alice** (Marketing) or **Bob**
(Product) to book pods, or as **Dana** (Platform team) to see **App
versions** and **Usage**. Four apps came with Roomer. They are live, but
marked *not reviewed*: they were installed before app review.

In a second terminal, review the apps and deploy them:

```sh
make            # check the tools, build the apps, review them with appfuzz
make deploy     # review again, then deploy: the shell reviews every version itself
```

`make` shows what appfuzz finds, app by app. On deploy the shell runs the
same review and decides. On App versions, every app now shows its verdict
and the findings.

| Verdict | What the shell does |
|---------|---------------------|
| pass | the version goes live |
| warn | the version goes live; the findings show up on App versions under *Needs a look* |
| fail | the version is refused, and the live version keeps serving |

### Let an agent add a feature

Ask an agent to add a 4-week trend to the stats widget, using only
`apps/kit/HOST-API.md`. Many write it like this, in
`apps/pod-stats/src/lib.rs`:

```rust
/// Average bookings per day over the last 28 days.
fn four_week_average(pod_id: u32, date: &str) -> String {
    let Ok(day) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { return "?".into() };
    let mut total = 0;
    for back in 1..=28 {
        let d = day - chrono::Days::new(back);
        let bookings = bookings_read::list_for_day(&d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        total += bookings.iter().filter(|b| b.pod_id == pod_id).count();
    }
    format!("{:.1}", total as f64 / 28.0)
}
```

called once per pod, with `chrono` added to `apps/pod-stats/Cargo.toml`.
Bump the version in `apps/pod-stats/app.toml` to `0.2.0` and run
`make deploy`:

```
pod-stats: fail, 1329 runs
  fail  n_plus_1       calls to appos:platform/bookings-read#list-for-day grow as 1 + 28·n with n = length
                       of the list appos:platform/pods-read#all returns; cost breaks its limit at n = 4
  fail  over_budget    appos:platform/handler#handle uses 1403 cost, the limit is 100
  fail  over_budget    appos:platform/handler#handle uses 1406.50 latency_ms, the limit is 200
  warn  serial_chain   appos:platform/handler#handle waits 1403 ms for 1403 host calls in a row ...
  ...
✖ pod-stats v0.2.0: Refused in review: 3 fail, 4 warn: n_plus_1, over_budget, ... pod-stats 0.1.0 keeps serving.
```

Roomer has four pods: the widget would break on the first day. Nobody had
to read the code to find that out.

To see why, go to the review's folder and ask appfuzz:

```sh
cd apps/target/review/pod-stats
appfuzz explain <run>        # the run id is in review.json
```

## How it works

```
            browser
               │
   ┌───────────▼─────────────────────────────────┐
   │ roomer (the shell)                          │
   │  routing · layout · <head> · sign-in · CSP  │
   │  app versions · usage · app review          │
   │                                             │
   │  ┌──────────────┐ ┌───────────┐ ┌─────────┐ │
   │  │ booking-grid │ │ pod-admin │ │pod-stats│ │  WebAssembly components
   │  └──────┬───────┘ └─────┬─────┘ └────┬────┘ │  (wasmtime)
   │         └── platform APIs (WIT) ─────┘      │  only granted ones linked
   │                      │                      │
   └──────────────────────▼──────────────────────┘
                       SQLite (owned by the shell)
```

- **Apps are WebAssembly components.** They have no file system, network or
  database of their own. Each request runs in a fresh instance with a memory
  and CPU budget. They reach the platform only through the functions in
  [HOST-API.md](apps/kit/HOST-API.md).
- **The manifest grants capabilities** (`apps/*/app.toml`):

  ```toml
  [app]
  id = "pod-stats"
  version = "0.1.0"
  author = "agent:claude (asked by Alice, Marketing)"

  [slots]
  widgets = ["home.aside"]

  [capabilities]
  grant = ["pods-read", "bookings-read"]
  ```

- **The sandbox checks the binary.** A compiled component lists exactly the
  interfaces its code calls. At deploy, the shell compares that list with
  the manifest and refuses anything ungranted, and it links only granted
  APIs into the app.
- **App review checks the behaviour.** The shell writes an appfuzz manifest
  for the app (`roomer manifest`): its grants and budget, plus what only the
  platform knows: what each call costs, which calls change data, the values
  the platform ever returns, and its business rules (one booking per pod
  and hour, hours 8 to 16). appfuzz runs the app thousands of times against
  that simulated platform. The app can't loosen its own review.
- **Casbin checks every call.** A call is allowed only if the app's manifest
  grants the capability *and* the signed-in user's role may use it.
- **Every request has a budget**: platform calls, time in them, CPU and
  memory. Over budget, the request is stopped, the slot shows "unavailable"
  and the rest of the page renders. A live version that fails 5 requests
  within a minute is rolled back to the previous one.
- **Business rules live in the platform**, not in apps. One booking per pod
  and hour, bookable hours, and "you can only cancel your own bookings"
  hold whichever app version runs.
- **The shell owns the page**: layout, navigation, `<head>`, scripts and
  styles. Apps return HTML and ask for a title. A strict Content Security
  Policy means app HTML can't run scripts.

## Layout

```
Makefile            check, build, review, deploy
platform/           the shell (the platform team's side)
  kit/              the platform kit: HOST-API.md, wit/appos.wit, sdk/
  src/runtime.rs    wasmtime sandbox and import checks
  src/platform.rs   platform API implementations and business rules
  src/review.rs     the appfuzz manifest and the review step
  src/registry.rs   deploy, review, rollback
  src/policy.rs     Casbin access policy (model and role rules in policy/)
  src/admin.rs      the App versions screen
  src/usage.rs      request metering and the Usage screens
  build.rs          builds the apps into the binary as the preinstalled ones
apps/               the apps (marketing's side)
  kit/              a copy of platform/kit
  booking-grid/  pod-admin/  pod-stats/  quick-book/
```

## Commands

```sh
make                # check the tools, build and review the apps
make review         # review only; exits non-zero if an app fails
make deploy         # review, then deploy every app to the running shell
make reset          # forget all data, bookings and deployed apps
make platform       # build the shell from platform/, apps built in
make kit            # copy platform/kit to apps/kit

roomer [serve] [--addr 127.0.0.1:8080] [--review-time 10s]
roomer deploy --manifest apps/x/app.toml --wasm apps/target/wasm32-wasip2/release/x.wasm
roomer manifest --manifest apps/x/app.toml --wasm x.wasm     # the appfuzz manifest
```

The sign-in is a demo: pick a user, no passwords.
