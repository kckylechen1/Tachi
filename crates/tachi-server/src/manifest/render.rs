use super::{DbRole, Manifest};

pub fn render_manifest(m: &Manifest) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Err(error) = writeln!(
        out,
        "tachi manifest v{}  generated_at={}  dbs={}",
        m.schema_version,
        m.generated_at,
        m.dbs.len()
    ) {
        tracing::warn!(error = %error, "failed to render manifest header");
    }
    for e in &m.dbs {
        if let Err(error) = writeln!(
            out,
            "  [{}] {}  owner={}  schema={}  vec={}  write={}  last={}  scope={}",
            role_str(&e.role),
            e.path,
            e.owner,
            e.schema_kind,
            e.vec_enabled,
            e.allow_write,
            e.last_classification,
            e.scope_hint,
        ) {
            tracing::warn!(error = %error, db_path = %e.path, "failed to render manifest entry");
        }
    }
    out
}

fn role_str(r: &DbRole) -> &'static str {
    match r {
        DbRole::Global => "global",
        DbRole::Project => "project",
        DbRole::Agent => "agent",
        DbRole::Foundry => "foundry",
        DbRole::Unknown => "unknown",
    }
}
