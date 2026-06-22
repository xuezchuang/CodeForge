pub fn normalize_display_path(path: &str) -> String {
    let trimmed = path.trim();

    if let Some(rest) = trimmed.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }

    if let Some(rest) = trimmed.strip_prefix(r"\\?\") {
        return rest.to_string();
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_display_path;

    #[test]
    fn removes_windows_verbatim_drive_prefix() {
        assert_eq!(
            normalize_display_path(r"\\?\D:\code\snowAgents"),
            r"D:\code\snowAgents"
        );
    }

    #[test]
    fn removes_windows_verbatim_unc_prefix() {
        assert_eq!(
            normalize_display_path(r"\\?\UNC\server\share\Game"),
            r"\\server\share\Game"
        );
    }

    #[test]
    fn leaves_normal_paths_unchanged() {
        assert_eq!(
            normalize_display_path(r"D:\code\snowAgents"),
            r"D:\code\snowAgents"
        );
    }
}
