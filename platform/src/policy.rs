//! Access policy, enforced by Casbin on every platform call.
//!
//! Linking is the first gate: an app can only call interfaces it was granted.
//! This is the second: each call is checked for this app, this user and this
//! capability, so e.g. a pod admin app can't let a non-admin create pods.

use std::sync::RwLock;

use casbin::{CoreApi, DefaultModel, Enforcer, MemoryAdapter, MgmtApi};
use futures::executor::block_on;

use crate::db::User;

const MODEL: &str = include_str!("../policy/model.conf");
const ROLE_RULES: &str = include_str!("../policy/roles.csv");

pub struct Policy {
    enforcer: RwLock<Enforcer>,
}

/// Rules of one type from a Casbin CSV, e.g. all `p` lines.
fn rules(csv: &str, ptype: &str) -> Vec<Vec<String>> {
    csv.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split(',').map(|f| f.trim().to_string()).collect::<Vec<_>>())
        .filter(|f| f.first().is_some_and(|t| t == ptype))
        .map(|f| f[1..].to_vec())
        .collect()
}

impl Policy {
    pub fn new(users: &[User]) -> anyhow::Result<Self> {
        block_on(async {
            let model = DefaultModel::from_str(MODEL).await?;
            let mut e = Enforcer::new(model, MemoryAdapter::default()).await?;
            e.add_policies(rules(ROLE_RULES, "p")).await?;
            let mut roles = rules(ROLE_RULES, "g");
            for u in users {
                roles.push(vec![u.id.clone(), if u.is_admin { "admin" } else { "member" }.to_string()]);
            }
            e.add_grouping_policies(roles).await?;
            Ok(Policy { enforcer: RwLock::new(e) })
        })
    }

    pub fn allows(&self, app: &str, user: &str, cap: &str) -> bool {
        let e = self.enforcer.read().unwrap_or_else(|e| e.into_inner());
        e.enforce((app, user, cap)).unwrap_or(false)
    }

    /// Replaces an app's grants with those of its newly active manifest.
    pub fn set_app_grants(&self, app: &str, grants: &[String]) -> anyhow::Result<()> {
        let mut e = self.enforcer.write().unwrap_or_else(|e| e.into_inner());
        block_on(async {
            e.remove_filtered_named_grouping_policy("g2", 0, vec![app.to_string()]).await?;
            let lines: Vec<Vec<String>> = grants.iter().map(|g| vec![app.to_string(), g.clone()]).collect();
            if !lines.is_empty() {
                e.add_named_grouping_policies("g2", lines).await?;
            }
            Ok(())
        })
    }

    /// The role rules as (role, capability) pairs, for the admin screen.
    pub fn role_rules(&self) -> Vec<(String, String)> {
        let e = self.enforcer.read().unwrap_or_else(|e| e.into_inner());
        e.get_policy().into_iter().filter_map(|r| Some((r.first()?.clone(), r.get(1)?.clone()))).collect()
    }

    /// The full policy in Casbin CSV form.
    pub fn dump(&self) -> String {
        let e = self.enforcer.read().unwrap_or_else(|e| e.into_inner());
        let mut out = String::new();
        for (ptype, rows) in
            [("p", e.get_policy()), ("g", e.get_grouping_policy()), ("g2", e.get_named_grouping_policy("g2"))]
        {
            for r in rows {
                out.push_str(&format!("{ptype}, {}\n", r.join(", ")));
            }
        }
        out
    }
}
