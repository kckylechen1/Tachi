use super::{DbRole, Manifest};

pub fn render_manifest(m: &Manifest) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "tachi manifest v{}  generated_at={}  dbs={}",
        m.schema_version,
        m.generated_at,
        m.dbs.len()
    );
    for e in &m.dbs {
        let _ = writeln!(
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
        );
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
