//! Deep-merge for `serde_norway::Value`. Used to apply a `DeploymentOverlay`
//! onto a base `ResourceDefinition` before reconciliation.
//!
//! Rules:
//! - Mappings: keys present in both are merged recursively; keys only in the
//!   overlay are inserted; keys only in the base are kept.
//! - Sequences: replaced entirely by the overlay (no element-wise merge).
//! - Scalars / nulls: overlay wins when present (non-null); base kept when
//!   overlay is null.
//! - Type mismatch (e.g. base mapping vs overlay scalar): overlay wins.

use serde_norway::Value;

/// Merge `overlay` into `base` in place. After this returns, `base` reflects
/// the merged result.
pub fn deep_merge(base: &mut Value, overlay: &Value) {
    if overlay.is_null() {
        return;
    }
    match (base, overlay) {
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (k, v) in overlay_map {
                if let Some(existing) = base_map.get_mut(k) {
                    deep_merge(existing, v);
                } else {
                    base_map.insert(k.clone(), v.clone());
                }
            }
        }
        (slot, value) => {
            *slot = value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        serde_norway::from_str(s).unwrap()
    }

    #[test]
    fn null_overlay_is_noop() {
        // Null overlay can't be expressed as a YAML doc the way the table cases are,
        // so it stays standalone: merging Null must leave base untouched.
        let mut base = yaml("a: 1\nb: 2");
        deep_merge(&mut base, &Value::Null);
        assert_eq!(base, yaml("a: 1\nb: 2"));
    }

    #[test]
    fn deep_merge_semantics() {
        // Each row: (base YAML, overlay YAML, expected merged YAML, why).
        let cases: &[(&str, &str, &str, &str)] = &[
            // Scalar overlay replaces the base scalar.
            ("a: 1", "a: 2", "a: 2", "scalar replace"),
            // Mapping keys merge (base-only keys kept, overlapping overwritten, new added).
            ("a: 1\nb: 2", "b: 99\nc: 3", "a: 1\nb: 99\nc: 3", "mapping key merge"),
            // Nested mappings recurse (sibling keys under api: preserved).
            ("api:\n  base_path: /v2/x\n  id_field: id", "api:\n  base_path: /v2/zen-x", "api:\n  base_path: /v2/zen-x\n  id_field: id", "nested recurse"),
            // Sequences are replaced wholesale, never element-merged.
            ("items: [1, 2, 3]", "items: [9]", "items: [9]", "sequence replace"),
            // Type mismatch (mapping vs scalar) → overlay wins.
            ("x:\n  a: 1", "x: scalar", "x: scalar", "type mismatch overlay wins"),
        ];
        for (base_s, overlay_s, expected_s, why) in cases {
            let mut base = yaml(base_s);
            deep_merge(&mut base, &yaml(overlay_s));
            assert_eq!(base, yaml(expected_s), "{why}");
        }
    }

    #[test]
    fn merge_into_discriminator_variant_fields() {
        // schema.variants[postgres].fields list is replaced by overlay's list (no element merge).
        let mut base = yaml("schema:\n  variants:\n    postgres:\n      fields:\n        - name: host\n          type: string");
        let overlay = yaml("schema:\n  variants:\n    postgres:\n      fields:\n        - name: host\n          type: string\n        - name: tls_required\n          type: boolean");
        deep_merge(&mut base, &overlay);
        let merged: Value = base;
        let fields = merged.get("schema").unwrap().get("variants").unwrap().get("postgres").unwrap().get("fields").unwrap();
        assert_eq!(fields.as_sequence().unwrap().len(), 2);
    }
}
