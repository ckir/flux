//! Operation and owner-instance IDs (cut 7a spec, "Operation ID and owner instance ID"; §18.1: collision-resistant).

/// A fresh random version-4 UUID as 32 lowercase hex digits, no dashes.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether `s` has exactly the shape `new_id` produces.
pub fn is_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_32_lowercase_hex_digits_and_fresh_each_time() {
        let a = new_id();
        assert!(is_id(&a), "{a}");
        assert_ne!(a, new_id());
    }

    #[test]
    fn is_id_rejects_every_other_shape() {
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
            "01234567-89ab-cdef-0123-456789abcdef",
        ] {
            assert!(!is_id(bad), "{bad}");
        }
    }

    #[test]
    fn is_id_rejects_a_longer_hex_string() {
        let id = new_id();
        assert!(!is_id(&format!("{id}0")), "33 hex digits");
        assert!(!is_id(&format!("{id}{id}")), "64 hex digits");
    }
}
