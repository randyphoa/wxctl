//! Runtime derived views over the static IR (`crate::ir`), re-homed from the
//! deleted owned `SchemaDefinition`/`ResourceDefinition` impls. Pure, wasm-safe.
use crate::ir::{FieldIr, FieldLocationIr, ResourceDefIr, SchemaBodyIr, VariantIr};
use std::collections::{HashMap, HashSet};

impl SchemaBodyIr {
    /// Variant groups already key-sorted at build time (Phase-1 D3): the pair
    /// slice preserves that order, so this just borrows each `VariantIr`.
    fn sorted_variants(&self) -> Vec<&VariantIr> {
        match self.variants {
            Some(pairs) => pairs.iter().map(|(_, v)| v).collect(),
            None => Vec::new(),
        }
    }
    pub fn all_fields(&self) -> Vec<&FieldIr> {
        let mut out: Vec<&FieldIr> = self.fields.iter().collect();
        let mut seen: HashSet<&str> = self.fields.iter().map(|f| f.name).collect();
        for variant in self.sorted_variants() {
            for field in variant.fields {
                if seen.insert(field.name) {
                    out.push(field);
                }
            }
        }
        out
    }
    pub fn fields_for_variant(&self, discriminator_value: &str) -> Vec<&FieldIr> {
        let mut out: Vec<&FieldIr> = self.fields.iter().collect();
        for variant in self.sorted_variants() {
            if variant.applies_to.contains(&discriminator_value) {
                for field in variant.fields {
                    out.push(field);
                }
            }
        }
        out
    }
    pub fn compute_state_fields(&self) -> Vec<String> {
        self.all_fields().into_iter().filter(|f| !matches!(f.location, FieldLocationIr::Computed | FieldLocationIr::LocalOnly)).map(|f| f.name.to_string()).collect()
    }
    pub fn build_field_mapping(&self) -> HashMap<String, String> {
        let mut mapping: HashMap<String, String> = HashMap::new();
        let mut ambiguous: HashSet<String> = HashSet::new();
        for field in self.all_fields() {
            if let Some(refs) = &field.references {
                for kind in std::iter::once(refs.resource).chain(refs.also_allows.iter().copied()) {
                    match mapping.get(kind) {
                        Some(existing) if existing != field.name => {
                            ambiguous.insert(kind.to_string());
                        }
                        Some(_) => {}
                        None => {
                            mapping.insert(kind.to_string(), field.name.to_string());
                        }
                    }
                }
            }
        }
        for kind in &ambiguous {
            mapping.remove(kind);
        }
        mapping
    }
    pub fn sensitive_paths(&self) -> Vec<String> {
        let root = [String::new()];
        let mut paths = Vec::new();
        collect_sensitive_paths(self.fields, &root, &mut paths);
        for variant in self.sorted_variants() {
            collect_sensitive_paths(variant.fields, &root, &mut paths);
        }
        paths
    }
}

fn collect_sensitive_paths(fields: &[FieldIr], prefixes: &[String], out: &mut Vec<String>) {
    for field in fields {
        let mut names: Vec<&str> = vec![field.name];
        if let Some(api) = field.api_field
            && api != field.name
        {
            names.push(api);
        }
        let mut paths: Vec<String> = Vec::new();
        for prefix in prefixes {
            for name in &names {
                let path = if prefix.is_empty() { (*name).to_string() } else { format!("{prefix}.{name}") };
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        if field.sensitive {
            for path in &paths {
                if !out.contains(path) {
                    out.push(path.clone());
                }
            }
        }
        if let Some(inner) = field.schema {
            collect_sensitive_paths(inner.fields, &paths, out);
        }
    }
}

impl ResourceDefIr {
    pub fn sensitive_paths(&self) -> Vec<String> {
        let base = self.schema.sensitive_paths();
        let list_field = self.reconciliation.discovery.list_field.unwrap_or("results");
        let mut out = base.clone();
        for path in &base {
            let enveloped = format!("entity.{}.{path}", self.name);
            out.push(format!("{list_field}.{enveloped}"));
            out.push(enveloped);
        }
        out
    }
}
