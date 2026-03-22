use crate::{
    events::canonicalize_actor,
    reducer::ReducerState,
};

use super::helpers::sha256_hex;

/// Verify actor passkey and return the UUID portion if valid.
pub fn verified_actor_uuid(reduced: &ReducerState, actor: Option<&str>, passkey: Option<&str>) -> Option<String> {
    let actor_str = actor?;
    let pk = passkey?;
    let actor_can = canonicalize_actor(actor_str);
    let stored_hash = reduced.actor_keys.get(&actor_can)?;
    let hash = sha256_hex(pk);
    if hash == *stored_hash {
        Some(crate::events::actor_uuid(&actor_can).to_string())
    } else {
        None
    }
}

/// Validate actor format: @<uuid>:<rig>:<model>
pub fn validate_actor_format(actor: &str) -> Result<(), String> {
    let actor = actor.strip_prefix('@').unwrap_or(actor);

    let parts: Vec<&str> = actor.split(':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "Invalid actor format. Expected @<uuid>:<rig>:<model>.\n\
             Got {} parts (expected 3).\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>",
            parts.len()
        ));
    }

    let (uuid_part, rig_part, model_part) = (parts[0], parts[1], parts[2]);

    if uuid::Uuid::parse_str(uuid_part).is_err() {
        return Err(format!(
            "Invalid UUID in actor format: '{}' is not a valid UUID.\n\
             Expected format: @<uuid>:<rig>:<model>\n\
             The UUID must be a full UUID v4.\n\
             \n\
             You provided: @{}:{}:{}\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>",
            uuid_part, uuid_part, rig_part, model_part
        ));
    }

    if rig_part.is_empty() {
        return Err(
            "Missing rig in actor format. Expected @<uuid>:<rig>:<model>.".to_string()
        );
    }

    // X actors use format @uuid:x.com:handle — no / required in third part.
    // AI agents use @uuid:rig:provider/model — require / in model.
    if rig_part != "x.com" && !model_part.contains('/') {
        return Err(
            "Invalid model format in actor.\n\
             Expected format: @<uuid>:<rig>:<provider/model>\n\
             Example: @7a3b9c2d...:claudecode:anthropic/claude-sonnet-4.5\n\
             \n\
             Generate a valid identity:\n\
             npx slugsocial identity --rig <name> --model <slug>"
                .to_string(),
        );
    }

    Ok(())
}
