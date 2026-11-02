//! The app manifest (`app.toml`): what an app is, where it plugs in, and
//! which platform capabilities it is granted.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub app: AppInfo,
    #[serde(default)]
    pub slots: Slots,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub limits: Limits,
}

/// Resource budgets, granted like capabilities: what the manifest asks for,
/// the app gets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Limits {
    /// I/O units one request may spend on platform calls (a read costs 1,
    /// a write 5).
    #[serde(default = "default_io_per_request")]
    pub io_per_request: u32,
}

pub const DEFAULT_IO_PER_REQUEST: u32 = 100;
pub const MAX_IO_PER_REQUEST: u32 = 10_000;

fn default_io_per_request() -> u32 {
    DEFAULT_IO_PER_REQUEST
}

impl Default for Limits {
    fn default() -> Self {
        Limits { io_per_request: DEFAULT_IO_PER_REQUEST }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Slots {
    /// URL paths this app serves. "/" matches only the root, any other route
    /// also matches everything below it.
    #[serde(default)]
    pub routes: Vec<String>,
    /// Named widget slots on shell pages, e.g. "home.aside".
    #[serde(default)]
    pub widgets: Vec<String>,
    /// A link in the top bar to the app's first route, with this label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub grant: Vec<String>,
}

/// Every capability the platform offers, with the text the admin screen shows.
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("pods-read", "Read the list of pods"),
    ("pods-write", "Create pods"),
    ("bookings-read", "Read bookings"),
    ("bookings-write", "Book slots and cancel own bookings"),
    ("identity", "See who is signed in"),
    ("log", "Write to the platform log"),
];

/// Widget slots the shell's pages offer.
pub const WIDGET_SLOTS: &[(&str, &str)] = &[("home.aside", "/")];

/// Path prefixes the shell keeps for itself.
pub const RESERVED_ROUTES: &[&str] = &["/admin", "/assets", "/_shell", "/_platform"];

/// How much harm a capability can do, so reviewers see the risky ones first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Only reads shared, non-personal data.
    Low,
    /// Learns who the user is.
    Personal,
    /// Changes data other people rely on.
    Writes,
}

impl Risk {
    pub fn class(self) -> &'static str {
        match self {
            Risk::Low => "risk-low",
            Risk::Personal => "risk-personal",
            Risk::Writes => "risk-writes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Risk::Low => "Reads only",
            Risk::Personal => "Personal data",
            Risk::Writes => "Changes data",
        }
    }
}

pub fn risk(capability: &str) -> Risk {
    match capability {
        "identity" => Risk::Personal,
        c if c.ends_with("-write") => Risk::Writes,
        _ => Risk::Low,
    }
}

pub fn describe(capability: &str) -> &'static str {
    CAPABILITIES.iter().find(|(c, _)| *c == capability).map(|(_, d)| *d).unwrap_or("Unknown capability")
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("invalid app.toml: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        let id = &self.app.id;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("app id {id:?} must be lowercase letters, digits and dashes"));
        }
        if self.app.version.is_empty() {
            return Err("version is required".into());
        }
        if self.slots.routes.is_empty() && self.slots.widgets.is_empty() {
            return Err("an app needs at least one route or widget slot".into());
        }
        for cap in &self.capabilities.grant {
            if !CAPABILITIES.iter().any(|(c, _)| c == cap) {
                return Err(format!("unknown capability {cap:?}"));
            }
        }
        for route in &self.slots.routes {
            if !route.starts_with('/') {
                return Err(format!("route {route:?} must start with /"));
            }
            if RESERVED_ROUTES.iter().any(|r| route_matches(r, route) || route_matches(route, r)) {
                return Err(format!("route {route} is reserved for the shell"));
            }
        }
        if self.limits.io_per_request == 0 || self.limits.io_per_request > MAX_IO_PER_REQUEST {
            return Err(format!("io_per_request must be between 1 and {MAX_IO_PER_REQUEST}"));
        }
        if let Some(label) = &self.slots.nav {
            if self.slots.routes.is_empty() {
                return Err("a nav link needs a route to point to".into());
            }
            if label.trim().is_empty() || label.chars().count() > 24 {
                return Err("a nav label must be 1 to 24 characters".into());
            }
        }
        for widget in &self.slots.widgets {
            if !WIDGET_SLOTS.iter().any(|(w, _)| w == widget) {
                return Err(format!("unknown widget slot {widget:?}"));
            }
        }
        Ok(())
    }

    /// The top bar link this app asks for: label and target.
    pub fn nav_link(&self) -> Option<(String, String)> {
        Some((self.slots.nav.clone()?, self.slots.routes.first()?.clone()))
    }

    pub fn serves(&self, path: &str) -> bool {
        self.slots.routes.iter().any(|r| route_matches(r, path))
    }
}

/// "/" matches only "/"; "/pods" matches "/pods" and "/pods/...".
pub fn route_matches(route: &str, path: &str) -> bool {
    if route == "/" {
        return path == "/";
    }
    path == route || path.strip_prefix(route).is_some_and(|rest| rest.starts_with('/'))
}

/// Two route sets overlap if any route of one matches any route of the other.
pub fn routes_overlap(a: &[String], b: &[String]) -> Option<String> {
    for x in a {
        for y in b {
            if route_matches(x, y) || route_matches(y, x) {
                return Some(x.clone());
            }
        }
    }
    None
}
