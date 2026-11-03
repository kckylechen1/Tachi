pub(super) fn shell_export_line(name: &str, value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("export {name}='{escaped}'")
}

pub(super) fn print_shell_exports(exports: &[(String, String)]) {
    for (name, value) in exports {
        println!("{}", shell_export_line(name, value));
    }
}
