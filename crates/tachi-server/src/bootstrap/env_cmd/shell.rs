pub(super) fn is_upper_snake_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
}

pub(super) fn shell_export_line(name: &str, value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("export {name}='{escaped}'")
}

pub(super) fn print_shell_exports(exports: &[(String, String)]) {
    for (name, value) in exports {
        println!("{}", shell_export_line(name, value));
    }
}

pub(super) fn upsert_env_secret(secrets: &mut Vec<(String, String)>, name: String, value: String) {
    if let Some((_, existing_value)) = secrets
        .iter_mut()
        .find(|(existing_name, _)| existing_name == &name)
    {
        *existing_value = value;
    } else {
        secrets.push((name, value));
    }
}
