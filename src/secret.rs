/*! A string which must never reach a receipt, a log line, or a plan description

It serializes as `"<redacted>"`, so a receipt written next to the installed system carries
no password — reverting never needs the value, only the names of what to undo. Reading a
receipt back therefore yields a `Secret` which is a placeholder, not a credential.
*/

#[derive(Clone, serde::Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

pub const REDACTED: &str = "<redacted>";

impl serde::Serialize for Secret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

impl Secret {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\"{REDACTED}\"")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn serializes_redacted() {
        let secret = Secret::new("hunter2");
        let json = serde_json::to_string(&secret).expect("serialize");
        assert_eq!(json, "\"<redacted>\"");
        assert_eq!(format!("{secret:?}"), "\"<redacted>\"");
        assert_eq!(secret.expose(), "hunter2");
    }
}
