use std::collections::HashMap;
use std::path::PathBuf;

/// Convert a HashMap to markdown bullet points, with keys as headers
pub fn hash_map_to_markdown(map: &HashMap<PathBuf, String>, default: &str) -> String {
    if map.is_empty() {
        return default.to_string();
    }
    let mut lines = Vec::new();
    for (path, content) in map.iter() {
        lines.push(format!("- `{}`: {}", path.display(), content));
    }
    lines.join("\n")
}

/// Convert a Vec to markdown bullet points
pub fn vec_to_markdown(items: &[String], default: &str) -> String {
    if items.is_empty() {
        return default.to_string();
    }
    items
        .iter()
        .map(|s| format!("- {}", s))
        .collect::<Vec<_>>()
        .join("\n")
}
