use crate::handlers::models::*;

/// Parse the `kinds` query parameter into a flat list of entity kind strings.
///
/// Returns the list of kind strings that should be visible, or an error if any
/// category name is invalid.
pub fn parse_kinds(kinds: &str) -> Result<Vec<&str>, String> {
    let cats: Vec<&str> = if kinds.trim().is_empty() {
        DEFAULT_VISIBLE_KINDS.split(',').map(str::trim).collect()
    } else {
        kinds.split(',').map(str::trim).collect()
    };

    for cat in &cats {
        if !VALID_KIND_CATEGORIES.contains(cat) {
            return Err(format!(
                "Invalid kind category '{}'. Valid values: {}",
                cat,
                VALID_KIND_CATEGORIES.join(", ")
            ));
        }
    }

    let mut visible = Vec::new();
    for cat in cats {
        match cat {
            "classes" => visible.extend_from_slice(KIND_CATEGORY_CLASSES),
            "interfaces" => visible.extend_from_slice(KIND_CATEGORY_INTERFACES),
            "functions" => visible.extend_from_slice(KIND_CATEGORY_FUNCTIONS),
            "other" => {
                // "other" means: any kind not explicitly in classes, interfaces, or functions.
                // We don't expand it here — the Cypher query will handle it via exclusion.
            }
            _ => {}
        }
    }

    Ok(visible)
}

/// Returns true if the "other" category is explicitly listed in `kinds`.
pub fn includes_other(kinds: &str) -> bool {
    if kinds.trim().is_empty() {
        return DEFAULT_VISIBLE_KINDS.contains("other");
    }
    kinds.split(',').any(|c| c.trim() == "other")
}

/// Parses a string direction into `knot::models::SubgraphDirection`.
pub fn parse_direction(direction: &str) -> knot::models::SubgraphDirection {
    match direction {
        "incoming" => knot::models::SubgraphDirection::Incoming,
        "outgoing" => knot::models::SubgraphDirection::Outgoing,
        _ => knot::models::SubgraphDirection::Both,
    }
}

/// Parses a comma-separated relationships string into a list of valid relationship strings.
pub fn parse_relationships(relationships: &str) -> Result<Vec<&str>, String> {
    let parsed: Vec<&str> = if relationships.trim().is_empty() {
        vec!["CALLS"]
    } else {
        relationships.split(',').map(|s| s.trim()).collect()
    };

    for rel in &parsed {
        if !VALID_RELATIONSHIPS.contains(rel) {
            return Err(format!(
                "Invalid relationship type '{}'. Valid types: {}",
                rel,
                VALID_RELATIONSHIPS.join(", ")
            ));
        }
    }

    Ok(parsed)
}
