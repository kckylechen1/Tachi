mod bindings;
mod command;
mod legacy;
mod materialize;
mod shell;
mod types;

pub(super) use command::run_env_command;

#[cfg(test)]
mod tests;
