//! HTTP path encoding for private rooms: `/r/{short}{slug}` (short is fixed width).

/// Byte length of the random `short` segment in `short/slug` room ids.
/// Must match room creation (`gen_short_id`) and [`super::paths`][] URL builders.
pub const ROOM_SHORT_ID_LEN: usize = 7;

/// `ab12cde/my-room` → `ab12cdemy-room` for a single `/r/…` path segment.
pub fn room_route_segment(room_id: &str) -> Option<String> {
    let (short, slug) = room_id.split_once('/')?;
    if short.len() != ROOM_SHORT_ID_LEN || short.is_empty() || slug.is_empty() {
        return None;
    }
    if !short
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z'))
    {
        return None;
    }
    Some(format!("{short}{slug}"))
}

/// `/r/{short}{slug}` path segment → `short/slug` wire id (inverse of [`room_route_segment`]).
pub fn room_id_from_route_segment(seg: &str) -> Option<String> {
    if seg.len() <= ROOM_SHORT_ID_LEN {
        return None;
    }
    let (short, slug) = seg.split_at(ROOM_SHORT_ID_LEN);
    if short.is_empty() || slug.is_empty() {
        return None;
    }
    if !short
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z'))
    {
        return None;
    }
    Some(format!("{short}/{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_room_segment() {
        let id = "9ab12cd/my-room";
        let seg = room_route_segment(id).unwrap();
        assert_eq!(seg, "9ab12cdmy-room");
        assert_eq!(room_id_from_route_segment(&seg).as_deref(), Some(id));
    }

    #[test]
    fn too_short_segment_rejected() {
        assert!(room_id_from_route_segment("9ab12cd").is_none());
    }
}
