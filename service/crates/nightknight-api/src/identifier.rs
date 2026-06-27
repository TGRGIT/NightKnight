//! Deriving a stable document identifier and its indexed metadata.
//!
//! Nightscout deduplicates on a content-derived identifier when the client doesn't
//! supply one. We reproduce that: a re-uploaded reading hashes to the same id and
//! becomes an update, not a duplicate point on the chart.

use serde_json::Value;

use nightknight_core::timeutil;
use nightknight_storage::Collection;

use crate::hashing::sha1_hex;

fn str_field(doc: &Value, key: &str) -> String {
    doc.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn num_field(doc: &Value, key: &str) -> Option<i64> {
    doc.get(key).and_then(|v| {
        v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
    })
}

/// Use the client's `identifier`/`_id` if present, else derive a stable one from the
/// document's key fields (per collection), matching Nightscout's dedup behaviour.
pub fn derive_identifier(c: Collection, doc: &Value) -> String {
    for key in ["identifier", "_id"] {
        if let Some(id) = doc.get(key).and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return id.to_string();
            }
        }
    }
    let basis = match c {
        Collection::Entries => format!(
            "{}|{}|{}",
            num_field(doc, "date").unwrap_or(0),
            str_field(doc, "type"),
            str_field(doc, "device"),
        ),
        Collection::Treatments => format!(
            "{}|{}|{}",
            str_field(doc, "created_at"),
            str_field(doc, "eventType"),
            num_field(doc, "date").unwrap_or(0),
        ),
        Collection::DeviceStatus => {
            format!("{}|{}", str_field(doc, "created_at"), str_field(doc, "device"))
        }
        // Profile/food/settings are low-volume; hash the whole body.
        _ => doc.to_string(),
    };
    sha1_hex(&basis)
}

/// Determine the primary epoch-ms time of a document, trying numeric fields first
/// then ISO strings, finally falling back to `now_ms`.
pub fn extract_mills(doc: &Value, now_ms: i64) -> i64 {
    if let Some(n) = num_field(doc, "date") {
        // A `date` in seconds (10 digits) rather than ms is rescaled by the same shared
        // heuristic validation uses, so the stored `mills` and the accept/reject
        // decision agree on the instant.
        return timeutil::normalize_epoch_ms(n);
    }
    if let Some(n) = num_field(doc, "mills") {
        return n;
    }
    for key in ["created_at", "dateString"] {
        if let Some(s) = doc.get(key).and_then(|v| v.as_str()) {
            if let Some(ms) = timeutil::parse_iso8601_ms(s) {
                return ms;
            }
        }
    }
    now_ms
}

/// The `type` of an entry document (`sgv`/`mbg`/`cal`), used for the indexed
/// `doc_type` column. `None` for non-entry collections.
pub fn extract_doc_type(c: Collection, doc: &Value) -> Option<String> {
    if c == Collection::Entries {
        doc.get("type").and_then(|v| v.as_str()).map(str::to_string)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A client-supplied identifier is respected verbatim.
    #[test]
    fn uses_client_identifier() {
        let doc = json!({ "identifier": "abc", "type": "sgv", "date": 1 });
        assert_eq!(derive_identifier(Collection::Entries, &doc), "abc");
    }

    /// Two identical SGV readings derive the same id (→ dedup), different ones don't.
    #[test]
    fn entries_dedup_by_date_type_device() {
        let a = json!({ "type": "sgv", "date": 1000, "device": "xDrip", "sgv": 100 });
        let b = json!({ "type": "sgv", "date": 1000, "device": "xDrip", "sgv": 105 });
        let c = json!({ "type": "sgv", "date": 2000, "device": "xDrip", "sgv": 100 });
        assert_eq!(
            derive_identifier(Collection::Entries, &a),
            derive_identifier(Collection::Entries, &b),
            "same date/type/device → same id"
        );
        assert_ne!(
            derive_identifier(Collection::Entries, &a),
            derive_identifier(Collection::Entries, &c),
            "different time → different id"
        );
    }

    /// `mills` comes from `created_at` when no numeric time is present.
    #[test]
    fn mills_from_created_at() {
        let doc = json!({ "eventType": "Meal Bolus", "created_at": "2023-11-14T22:13:19.000Z" });
        assert_eq!(extract_mills(&doc, 0), 1_699_999_999_000);
    }

    /// A 10-digit seconds `date` is scaled to milliseconds.
    #[test]
    fn seconds_date_is_scaled() {
        let doc = json!({ "type": "sgv", "date": 1_699_999_999i64 });
        assert_eq!(extract_mills(&doc, 0), 1_699_999_999_000);
    }
}
