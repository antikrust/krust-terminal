//! `KEY=VALUE` files — Moloch's `moloch.env`, acolyte's `.env` — read and written the way Krust's
//! Moloch settings and the servers' own loaders do: blank lines and `#` comments skipped, an
//! optional leading `export `, surrounding quotes stripped.

use std::collections::HashMap;
use std::path::Path;

/// The file's values, or nothing when it can't be read.
pub fn read(path: &Path) -> HashMap<String, String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        values.insert(key.to_owned(), value.to_owned());
    }
    values
}

/// Sets `values` in the file, keeping every other line (and comment) as it was and the previous
/// file beside it as `<name>.bak`. These files hold tokens, so only you can read them.
pub fn write(path: &Path, values: &[(&str, String)]) -> std::io::Result<()> {
    let previous = std::fs::read_to_string(path).unwrap_or_default();
    if !previous.is_empty() {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak");
        let _ = std::fs::write(backup, &previous);
    }
    let mut written = vec![false; values.len()];
    let mut lines: Vec<String> = previous
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let key = trimmed
                .strip_prefix("export ")
                .unwrap_or(trimmed)
                .split_once('=')
                .map(|(key, _)| key.trim().to_owned());
            match key.and_then(|key| values.iter().position(|(wanted, _)| *wanted == key)) {
                Some(index) => {
                    written[index] = true;
                    format!("{}={}", values[index].0, values[index].1)
                }
                None => line.to_owned(),
            }
        })
        .collect();
    for (index, (key, value)) in values.iter().enumerate() {
        if !written[index] {
            lines.push(format!("{key}={value}"));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lines.join("\n") + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
