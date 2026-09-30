//! The operation state (cut 7a spec, "Operation state, `format_version` 1"): one JSON object - a tree operation's
//! `manifest`, or a single-file operation's adjacent record - its codec, its crash-safe write, and the names Flux gives
//! it. `format_version` is judged before any other key, and nothing is parsed heuristically (§193).

use crate::ids::is_id;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The version this binary writes and reads. 7b bumps it and reads this one too.
pub const FORMAT_VERSION: u64 = 1;
/// The longest state record read. A version-1 record is well under 1 KiB; anything longer is not one.
pub const STATE_LIMIT: usize = 64 * 1024;
/// A takeover's value for a prior holder's field it could not read (spec:10705-10709).
pub const UNREADABLE: &str = "unreadable";
/// Every key of a version-1 record: exactly these, no more and no fewer.
const KEYS: [&str; 8] = [
    "format_version",
    "operation_id",
    "kind",
    "destination_root",
    "state",
    "superseded_by",
    "created_at",
    "takeover",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tree,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OpState {
    Created,
    Transferring,
    Failed,
    Completed,
    Abandoned,
}

impl OpState {
    /// §21.1: CREATED, TRANSFERRING and FAILED are resumable. COMPLETED and ABANDONED are terminal (the design's
    /// ABANDONED ruling).
    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Created | Self::Transferring | Self::Failed)
    }

    /// The spec's spelling, as the JSON holds it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Transferring => "TRANSFERRING",
            Self::Failed => "FAILED",
            Self::Completed => "COMPLETED",
            Self::Abandoned => "ABANDONED",
        }
    }
}

/// The §240.5 takeover record: the prior holder's fields as read, and the takeover's own time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Takeover {
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub last_heartbeat_wall_time: String,
    pub creation_wall_time: String,
}

impl Takeover {
    /// 7a takes over only an UNREADABLE lock (§240.5 is for `TARGET_LOCK_UNCERTAIN`), so every prior field is
    /// `unreadable`.
    pub fn of_unreadable(creation_wall_time: u64) -> Self {
        Self {
            operation_id: UNREADABLE.to_string(),
            owner_instance_id: UNREADABLE.to_string(),
            boot_session_id: UNREADABLE.to_string(),
            last_heartbeat_wall_time: UNREADABLE.to_string(),
            creation_wall_time: creation_wall_time.to_string(),
        }
    }
}

/// One operation's state, version 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationState {
    pub format_version: u64,
    pub operation_id: String,
    pub kind: Kind,
    /// The destination as the operator gave it, in its display form: informational, lossy for a path that is not
    /// valid Unicode, and never read back for a decision (decision 5).
    pub destination_root: String,
    pub state: OpState,
    pub superseded_by: Option<String>,
    /// Nanoseconds since the Unix epoch, UTC, as decimal ASCII.
    pub created_at: String,
    pub takeover: Option<Takeover>,
}

/// Why a state record cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// Unreadable or malformed: `STATE_CORRUPT`.
    Corrupt(String),
    /// A `format_version` this binary does not know: `INCOMPATIBLE_STATE`.
    Incompatible(u64),
}

impl OperationState {
    /// A new operation's state, `CREATED`.
    pub fn created(
        operation_id: &str,
        kind: Kind,
        destination_root: &Path,
        created_at: u64,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            operation_id: operation_id.to_string(),
            kind,
            destination_root: destination_root.display().to_string(),
            state: OpState::Created,
            superseded_by: None,
            created_at: created_at.to_string(),
            takeover: None,
        }
    }

    /// The record's bytes: the keys in `KEYS`' order, compact JSON.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an operation state always serializes")
    }
}

/// Decode a state record. In order: the size; JSON; an object; `format_version` (before any other key, F4); exactly
/// the eight keys; their types; their values.
pub fn decode(bytes: &[u8]) -> Result<OperationState, Unusable> {
    use serde_json::Value;
    if bytes.len() > STATE_LIMIT {
        return Err(Unusable::Corrupt(format!("longer than {STATE_LIMIT} bytes")));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| Unusable::Corrupt(format!("not JSON: {e}")))?;
    let Value::Object(map) = &value else {
        return Err(Unusable::Corrupt("not a JSON object".to_string()));
    };
    let version = match map.get("format_version") {
        Some(v) => v.as_u64().ok_or_else(|| {
            Unusable::Corrupt(format!("format_version is not a whole number: {v}"))
        })?,
        None => return Err(Unusable::Corrupt("no format_version".to_string())),
    };
    if version != FORMAT_VERSION {
        return Err(Unusable::Incompatible(version));
    }
    if let Some(key) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(Unusable::Corrupt(format!("unknown key {key}")));
    }
    if let Some(key) = KEYS.iter().find(|k| !map.contains_key(**k)) {
        return Err(Unusable::Corrupt(format!("missing key {key}")));
    }
    let state: OperationState =
        serde_json::from_value(value).map_err(|e| Unusable::Corrupt(format!("malformed: {e}")))?;
    validate(&state).map_err(Unusable::Corrupt)?;
    Ok(state)
}

fn validate(s: &OperationState) -> Result<(), String> {
    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    if !is_id(&s.operation_id) {
        return Err(format!("operation_id is not an id: {}", s.operation_id));
    }
    if let Some(by) = &s.superseded_by
        && !is_id(by)
    {
        return Err(format!("superseded_by is not an id: {by}"));
    }
    if !digits(&s.created_at) {
        return Err(format!("created_at is not decimal nanoseconds: {}", s.created_at));
    }
    if let Some(t) = &s.takeover {
        let id_or_unreadable = |v: &str| is_id(v) || v == UNREADABLE;
        if !id_or_unreadable(&t.operation_id) || !id_or_unreadable(&t.owner_instance_id) {
            return Err("the takeover's prior holder is neither an id nor unreadable".to_string());
        }
        if t.boot_session_id.is_empty() {
            return Err("the takeover's boot_session_id is empty".to_string());
        }
        if !(digits(&t.last_heartbeat_wall_time) || t.last_heartbeat_wall_time == UNREADABLE) {
            return Err("the takeover's last_heartbeat_wall_time is not a time".to_string());
        }
        if !digits(&t.creation_wall_time) {
            return Err("the takeover's creation_wall_time is not a time".to_string());
        }
    }
    Ok(())
}

/// Now, in nanoseconds since the Unix epoch (UTC), as the lock record and the state hold it.
pub fn wall_time_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn created() -> OperationState {
        OperationState::created(&id(1), Kind::Tree, Path::new("/d"), 5)
    }

    #[test]
    fn a_state_encodes_to_the_documented_json_and_back() {
        let s = created();
        let text = String::from_utf8(s.encode()).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"format_version":1,"operation_id":"{}","kind":"tree","destination_root":"/d","state":"CREATED","superseded_by":null,"created_at":"5","takeover":null}}"#,
                id(1)
            )
        );
        assert_eq!(decode(text.as_bytes()), Ok(s));
    }

    #[test]
    fn every_state_and_kind_has_its_spec_spelling_and_resumability() {
        for (state, text, resumable) in [
            (OpState::Created, "CREATED", true),
            (OpState::Transferring, "TRANSFERRING", true),
            (OpState::Failed, "FAILED", true),
            (OpState::Completed, "COMPLETED", false),
            (OpState::Abandoned, "ABANDONED", false),
        ] {
            assert_eq!(state.as_str(), text);
            assert_eq!(state.is_resumable(), resumable, "{text}");
            let s = OperationState { state, ..created() };
            let json = String::from_utf8(s.encode()).unwrap();
            assert!(json.contains(&format!(r#""state":"{text}""#)), "{json}");
            assert_eq!(decode(&s.encode()), Ok(s));
        }
        let f = OperationState { kind: Kind::File, ..created() };
        assert!(String::from_utf8(f.encode()).unwrap().contains(r#""kind":"file""#));
    }

    #[test]
    fn format_version_is_judged_before_any_other_key() {
        assert_eq!(
            decode(br#"{"format_version":2,"anything":[1,2]}"#),
            Err(Unusable::Incompatible(2))
        );
        assert!(matches!(decode(br#"{"kind":"tree"}"#), Err(Unusable::Corrupt(_))), "none");
        assert!(matches!(decode(br#"{"format_version":"1"}"#), Err(Unusable::Corrupt(_))), "text");
        assert!(matches!(decode(br#"{"format_version":1.0}"#), Err(Unusable::Corrupt(_))), "float");
    }

    #[test]
    fn a_version_1_record_has_exactly_its_eight_keys() {
        let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
        v["extra"] = serde_json::json!(true);
        assert!(
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w.contains("extra"))
        );
        let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
        v.as_object_mut().unwrap().remove("superseded_by");
        assert!(
            matches!(decode(v.to_string().as_bytes()), Err(Unusable::Corrupt(w)) if w.contains("superseded_by"))
        );
    }

    #[test]
    fn malformed_values_are_corrupt() {
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v: serde_json::Value = serde_json::from_slice(&created().encode()).unwrap();
            edit(&mut v);
            decode(v.to_string().as_bytes())
        };
        for (what, r) in [
            ("an operation_id that is not an id", bad(&|v| v["operation_id"] = "../x".into())),
            ("a superseded_by that is not an id", bad(&|v| v["superseded_by"] = "x".into())),
            ("a created_at that is not decimal", bad(&|v| v["created_at"] = "12a".into())),
            ("an unknown state", bad(&|v| v["state"] = "DONE".into())),
            ("an unknown kind", bad(&|v| v["kind"] = "dir".into())),
            (
                "a takeover missing a key",
                bad(&|v| v["takeover"] = serde_json::json!({"operation_id": "unreadable"})),
            ),
        ] {
            assert!(matches!(r, Err(Unusable::Corrupt(_))), "{what}: {r:?}");
        }
        for (what, bytes) in
            [("not JSON", &b"{"[..]), ("not an object", &b"[1]"[..]), ("empty", &b""[..])]
        {
            assert!(matches!(decode(bytes), Err(Unusable::Corrupt(_))), "{what}");
        }
        assert!(
            matches!(decode(&vec![b' '; STATE_LIMIT + 1]), Err(Unusable::Corrupt(_))),
            "oversized"
        );
    }

    #[test]
    fn an_unreadable_takeover_round_trips() {
        let t = Takeover::of_unreadable(9);
        assert_eq!(
            (
                t.operation_id.as_str(),
                t.last_heartbeat_wall_time.as_str(),
                t.creation_wall_time.as_str()
            ),
            (UNREADABLE, UNREADABLE, "9")
        );
        let s = OperationState { takeover: Some(t), superseded_by: Some(id(2)), ..created() };
        assert_eq!(decode(&s.encode()), Ok(s));
    }
}
