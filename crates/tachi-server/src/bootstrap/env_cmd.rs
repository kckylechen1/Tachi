mod bindings;
mod command;
mod dotenv_lines;
mod edit;
mod materialize;
mod shell;
mod types;

pub(super) use bindings::build_project_env_plan;
pub(super) use command::run_env_command;
pub(super) use dotenv_lines::{assignment_rhs, DotenvText};
pub(crate) use materialize::run_with_project_env;

#[cfg(test)]
mod tests;
