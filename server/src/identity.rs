//! Usernames and agent delegate ids. Wire JSON and query params use **stored form only** (no `@` / `@@`).
//! The HTTP layer validates here; the reducer does not rewrite identity. For humans, `@name` / `@@agent`
//! appear only in HTML (see `html::authorship_address`).

/// Parse username from query/body: trim, lowercase. `@` is not allowed (use `tommy`, not `@tommy`).
pub fn parse_username(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("username must not be empty".to_string());
    }
    if s.contains('@') {
        return Err(
            "username must not contain '@' — use stored form (e.g. `tommy`)".to_string(),
        );
    }
    let u = s.to_lowercase();
    validate_username_naked(&u)?;
    Ok(u)
}

fn validate_username_naked(u: &str) -> Result<(), String> {
    if u.len() > 32 {
        return Err("username must be 1-32 characters".to_string());
    }
    if !u
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("username must be lowercase alphanumeric with '-' or '_' only".to_string());
    }
    Ok(())
}

/// Parse agent id: trim, lowercase. Must be `uuid:rig:provider/model` with no `@`.
pub fn parse_agent(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("agent id must not be empty".to_string());
    }
    if s.contains('@') {
        return Err(
            "agent id must not contain '@' — use `uuid:rig:provider/model`".to_string(),
        );
    }
    let a = s.to_lowercase();
    validate_agent_naked(&a)?;
    Ok(a)
}

fn validate_agent_naked(a: &str) -> Result<(), String> {
    let parts: Vec<&str> = a.split(':').collect();
    if parts.len() != 3 {
        return Err("agent must be <uuid>:<rig>:<provider/model>".to_string());
    }
    let (uuid_part, rig_part, model_part) = (parts[0], parts[1], parts[2]);
    if uuid::Uuid::parse_str(uuid_part).is_err() {
        return Err("agent uuid must be a valid UUID v4".to_string());
    }
    if rig_part.trim().is_empty() {
        return Err("agent rig must be non-empty".to_string());
    }
    if !model_part.contains('/') {
        return Err("agent model must be <provider/model>".to_string());
    }
    Ok(())
}
