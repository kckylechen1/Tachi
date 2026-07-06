mod alias;
mod home;
mod named;
mod reconcile;
mod symlink;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use alias::validate_project_db_relpath;
pub(crate) use alias::{
    plan_c_dir_name_from_root, plan_c_global_db_path, plan_c_legacy_dir_name_from_root,
    plan_c_project_root_from_local_db, resolve_project_db_path,
};
pub(crate) use home::tachi_home;
pub(crate) use named::{list_named_projects, named_project_for_db_path, named_project_from_path};
pub(crate) use reconcile::{reconcile_plan_c_alias_drift, PlanCReconcileAction};
pub(crate) use symlink::{
    ensure_plan_c_symlink, plan_c_split_brain, plan_c_split_brain_for_local_db,
};
pub(crate) use types::{PlanCLinkOutcome, PlanCSplitBrain};
