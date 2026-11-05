use serde_json::Value;

const MAX_CONFIGURATION_BYTES: usize = 256 * 1024;
const PART_BYTES: usize = 60_000;
const PART_COUNT_KEY: &str = "neurox_configuration_parts";

pub fn configuration_entries(configuration: &Value) -> Result<Vec<(String, String)>, String> {
    if configuration["schemaVersion"] != 1 || !configuration["workspace"].is_object() {
        return Err("invalid recording configuration schema".into());
    }
    let text = serde_json::to_string(configuration).map_err(|error| error.to_string())?;
    if text.len() > MAX_CONFIGURATION_BYTES {
        return Err("recording configuration exceeds 256 KiB".into());
    }
    // Keep the existing v1 metadata encoding, including its u16 string lengths.
    let mut entries = Vec::new();
    let mut remaining = text.as_str();
    while !remaining.is_empty() {
        let mut end = remaining.len().min(PART_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        entries.push((
            format!("neurox_configuration_{}", entries.len()),
            remaining[..end].into(),
        ));
        remaining = &remaining[end..];
    }
    entries.push((PART_COUNT_KEY.into(), entries.len().to_string()));
    Ok(entries)
}

pub fn read_configuration(metadata: &[(String, String)]) -> Result<Option<Value>, String> {
    let counts: Vec<_> = metadata
        .iter()
        .filter(|(key, _)| key == PART_COUNT_KEY)
        .collect();
    if counts.is_empty() {
        return Ok(None);
    }
    if counts.len() != 1 {
        return Err("duplicate recording configuration header".into());
    }
    let count = counts[0]
        .1
        .parse::<usize>()
        .map_err(|_| "invalid recording configuration part count")?;
    if !(1..=MAX_CONFIGURATION_BYTES.div_ceil(PART_BYTES)).contains(&count) {
        return Err("invalid recording configuration part count".into());
    }
    let mut text = String::new();
    for index in 0..count {
        let name = format!("neurox_configuration_{index}");
        let parts: Vec<_> = metadata.iter().filter(|(key, _)| key == &name).collect();
        if parts.len() != 1 {
            return Err("missing or duplicate recording configuration part".into());
        }
        text.push_str(&parts[0].1);
        if text.len() > MAX_CONFIGURATION_BYTES {
            return Err("recording configuration exceeds 256 KiB".into());
        }
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("invalid recording configuration: {error}"))?;
    if value["schemaVersion"] != 1 || !value["workspace"].is_object() {
        return Err("unsupported recording configuration schema".into());
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configuration_round_trips_unicode_across_metadata_parts() {
        let value = json!({"schemaVersion": 1, "workspace": {"notes": "实验参数".repeat(8_000)}});
        let entries = configuration_entries(&value).unwrap();
        assert!(entries.len() > 2);
        assert!(entries
            .iter()
            .all(|(_, text)| text.len() <= u16::MAX as usize));
        assert_eq!(read_configuration(&entries).unwrap(), Some(value));
    }

    #[test]
    fn legacy_headers_are_supported_but_broken_configurations_are_rejected() {
        assert_eq!(read_configuration(&[]).unwrap(), None);
        let value = json!({"schemaVersion": 1, "workspace": {}});
        let entries = configuration_entries(&value).unwrap();
        let mut duplicate = entries.clone();
        duplicate.push(entries[0].clone());
        assert!(read_configuration(&duplicate).is_err());
        assert!(read_configuration(&entries[1..]).is_err());
        assert!(configuration_entries(&json!({"schemaVersion": 2, "workspace": {}})).is_err());
        assert!(configuration_entries(&json!({"schemaVersion": 1, "workspace": {"notes": "x".repeat(MAX_CONFIGURATION_BYTES)}})).is_err());
    }
}
