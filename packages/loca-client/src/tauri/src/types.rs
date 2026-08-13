use serde::Deserialize;

// Window alignment for `position_window`. Sidecar / server status types have
// moved to loca-core (returned over HTTP); the client no longer mirrors them.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum WindowAlignment {
    Left,
    Center,
    Right,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn window_alignment_deserializes_from_lowercase_strings() {
        let left: WindowAlignment = serde_json::from_str(r#""left""#).unwrap();
        let center: WindowAlignment = serde_json::from_str(r#""center""#).unwrap();
        let right: WindowAlignment = serde_json::from_str(r#""right""#).unwrap();
        assert!(matches!(left, WindowAlignment::Left));
        assert!(matches!(center, WindowAlignment::Center));
        assert!(matches!(right, WindowAlignment::Right));
    }

    #[test]
    fn window_alignment_rejects_unknown_values() {
        assert!(serde_json::from_str::<WindowAlignment>(r#""middle""#).is_err());
        assert!(serde_json::from_str::<WindowAlignment>(r#""LEFT""#).is_err());
    }
}
