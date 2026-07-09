use crate::commands::resources::{build_rows, deploy_tags, product_name, render_rows_text};
use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{Color, Role, Theme, vt_enabled};
use crate::output::resource_format::{ResourceFormat, print_json, print_yaml, render};
use anyhow::Result;
use serde::Serialize;
use wxctl_providers::dependency_graph::deployment_support;
use wxctl_schema::explain::type_label;
use wxctl_schema::explain::{Authoring, ExplainField, ExplainView, KindSummary};

pub fn execute(kind: Option<&str>, format: ResourceFormat) -> Result<()> {
    match kind {
        Some(kind) => {
            let view = wxctl_schema::explain_kind(kind)?;
            render(&view, format, |_| render_table(&view))
        }
        None => {
            let overview = AuthoringOverview { authoring: wxctl_schema::authoring_overview(), kinds: wxctl_schema::list_kinds(None, None) };
            match format {
                ResourceFormat::Json => print_json(&overview)?,
                ResourceFormat::Yaml => print_yaml(&overview)?,
                ResourceFormat::Table | ResourceFormat::Markdown => {
                    let rows = build_rows(None, None)?;
                    if matches!(format, ResourceFormat::Table) {
                        let theme = Theme::resolve(None);
                        let width = Panel::resolve_width();
                        let glyphs = GlyphSet::resolve(vt_enabled() && !theme.is_plain());
                        render_authoring(&Panel::new(theme, width, glyphs), &overview.authoring);
                    }
                    render_rows_text(&rows, format);
                }
            }
            Ok(())
        }
    }
}

/// The no-kind `explain` payload: the cross-kind authoring conventions plus the
/// full kind catalog. Serialized directly for `-o json` / `-o yaml`; the text
/// forms print the authoring block (Table only) then the shared catalog view.
#[derive(Serialize)]
struct AuthoringOverview {
    authoring: Authoring,
    kinds: Vec<KindSummary>,
}

/// `explain`'s bespoke detail layout, sharing the panel visual language (▌
/// section bars, dim rules, color-coded verbs/flags). Width + colors come from
/// the resolved `Panel` (deterministic for snapshots).
fn render_table(view: &ExplainView) {
    let theme = Theme::resolve(None);
    let width = Panel::resolve_width();
    let glyphs = GlyphSet::resolve(vt_enabled() && !theme.is_plain());
    let panel = Panel::new(theme, width, glyphs);

    // ── header: kind · product · deployment, then the id field ──
    let deployment = deployment_support(&view.kind).join(", ");
    println!();
    println!("  {}   {} {} {}", panel.paint(Role::Heading, &view.kind), panel.paint(Role::Meta, product_name(&view.service)), panel.g("dot"), deploy_tags(&panel, &deployment, 0));
    println!("  {}", panel.paint(Role::Meta, &format!("id field {} {}", panel.g("dot"), view.id_field)));

    // ── endpoints, lifecycle order, verbs color-coded ──
    section(&panel, "Endpoints", None);
    let mut endpoints: Vec<(&str, &str, &str)> = vec![("create", "POST", &view.endpoints.create), ("get", "GET", &view.endpoints.get)];
    if let Some(list) = &view.endpoints.list {
        endpoints.push(("list", "GET", list));
    }
    if let Some(update) = &view.endpoints.update {
        endpoints.push(("update", view.endpoints.update_method.as_deref().unwrap_or("PATCH"), update));
    }
    endpoints.push(("delete", "DELETE", &view.endpoints.delete));
    for (label, verb, path) in endpoints {
        println!("    {}   {}   {}", panel.paint(Role::Meta, &format!("{:<6}", label)), panel.paint_color(verb_color(verb), &format!("{:<6}", verb)), path);
    }

    // ── fields, required first, with status + locked flags ──
    section(&panel, "Fields", Some("required first"));
    let name_w = view.fields.iter().map(|f| f.name.len()).max().unwrap_or(4);
    let type_w = view.fields.iter().map(|f| type_label(f).len()).max().unwrap_or(4);
    let mut fields: Vec<&ExplainField> = view.fields.iter().collect();
    fields.sort_by_key(|f| field_rank(f));
    for f in &fields {
        let (status, color) = field_status(f);
        let locked = if f.immutable { panel.paint(Role::Meta, "locked") } else { " ".repeat(6) };
        println!("    {:<name_w$}   {}   {}   {}   {}", f.name, panel.paint(Role::Meta, &format!("{:<type_w$}", type_label(f))), panel.paint_color(color, &format!("{:<8}", status)), locked, panel.paint(Role::Meta, &f.location));
    }

    // ── dependencies as field ──▶ target arrows ──
    section(&panel, "Dependencies", None);
    if view.dependencies.is_empty() {
        println!("    {}", panel.paint(Role::Meta, "(none)"));
    } else {
        let dep_w = view.dependencies.iter().map(|d| d.field.len()).max().unwrap_or(4);
        for d in &view.dependencies {
            let req = if d.required { panel.paint(Role::Success, "required") } else { panel.paint(Role::Meta, "optional") };
            println!("    {:<dep_w$}  {}  {}   {}", d.field, panel.paint(Role::Meta, panel.g("arrow")), panel.paint(Role::Active, &format!("${{{}.<ref_name>}}", d.target_kind)), req);
        }
    }

    // ── prompt notes (optional) ──
    if !view.prompt_notes.is_empty() {
        section(&panel, "Notes", None);
        for note in &view.prompt_notes {
            println!("    {} {}", panel.paint(Role::Meta, panel.g("bullet")), panel.paint(Role::Meta, note));
        }
    }

    // ── authoring conventions ──
    render_authoring(&panel, &view.authoring);
    println!();
}

/// Render the `▌ Authoring` section: the envelope / `ref_name` / reference-syntax
/// conventions. Shared by the per-kind view and the no-kind config-model overview.
fn render_authoring(panel: &Panel, authoring: &Authoring) {
    section(panel, "Authoring", None);
    println!("    {} {}", panel.paint(Role::Meta, "envelope  "), panel.paint(Role::Meta, authoring.envelope));
    println!("    {} {}", panel.paint(Role::Meta, "ref_name  "), panel.paint(Role::Meta, authoring.ref_name));
    println!("    {} {}", panel.paint(Role::Meta, "reference "), panel.paint(Role::Meta, authoring.reference_syntax));
}

/// Print a `▌ Title   (hint)` section header via the panel.
fn section(panel: &Panel, title: &str, hint: Option<&str>) {
    println!();
    println!("{}", panel.section(title, hint));
}

/// Semantic color for an HTTP verb, mirroring the plan/apply decision palette.
fn verb_color(verb: &str) -> Color {
    match verb {
        "POST" => Color::Green,
        "GET" => Color::Blue,
        "PUT" | "PATCH" => Color::Yellow,
        "DELETE" => Color::Red,
        _ => Color::Reset,
    }
}

/// Sort rank so required inputs come first, then optional, then computed fields.
fn field_rank(f: &ExplainField) -> u8 {
    if f.location == "Computed" {
        2
    } else if f.required {
        0
    } else {
        1
    }
}

/// Status label + color: computed fields are derived (dim), required inputs green.
fn field_status(f: &ExplainField) -> (&'static str, Color) {
    if f.location == "Computed" {
        ("computed", Color::Dim)
    } else if f.required {
        ("required", Color::Green)
    } else {
        ("optional", Color::Dim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(fields: &'a [ExplainField], name: &str) -> &'a ExplainField {
        fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("field {name} present"))
    }

    /// The structured view must surface the per-field metadata an agent authors
    /// against — type, element type, default, and validation — not just the
    /// name/required/immutable/location the old projection exposed.
    #[test]
    fn enriches_scalar_array_and_validation() {
        let v = wxctl_schema::explain_kind("s3_bucket").unwrap();

        let name = field(&v.fields, "name");
        assert_eq!(name.field_type, "string");
        assert!(name.validation.as_ref().and_then(|val| val.pattern.as_ref()).is_some(), "name carries its regex");

        let storage_class = field(&v.fields, "storage_class");
        assert!(storage_class.default.is_some(), "default is exposed");

        let tags = field(&v.fields, "tags");
        assert_eq!(tags.field_type, "array");
        assert_eq!(tags.item_type.as_deref(), Some("string"), "array element type is exposed");
    }

    /// Closed enums and nested object sub-fields must both reach the view — the
    /// nested case is what lets an agent author an `object` field correctly.
    #[test]
    fn surfaces_enums_and_recurses_into_objects() {
        let v = wxctl_schema::explain_kind("project").unwrap();

        let type_field = field(&v.fields, "type");
        assert!(type_field.allowed_values.as_ref().is_some_and(|vals| vals.iter().any(|s| s == "wx")), "top-level enum surfaces");

        let storage = field(&v.fields, "storage");
        let nested = storage.fields.as_ref().expect("object field recurses into sub-fields");
        let nested_type = field(nested, "type");
        assert!(nested_type.required, "nested required flag preserved");
        assert!(nested_type.allowed_values.is_some(), "nested enum surfaces");
    }

    /// A reference field must carry the literal `${kind.<ref_name>}` to author, so
    /// the agent learns the grammar without prior knowledge of it.
    #[test]
    fn reference_fields_carry_the_literal_to_author() {
        let v = wxctl_schema::explain_kind("s3_bucket").unwrap();
        let connection = field(&v.fields, "connection");
        assert_eq!(connection.reference.as_deref(), Some("${storage_connection.<ref_name>}"));

        // Plain scalar fields carry no reference.
        assert!(field(&v.fields, "region").reference.is_none());
    }

    /// The envelope / ref_name / reference conventions are always present — they
    /// are the cross-kind knowledge an agent can't get from any single schema.
    #[test]
    fn authoring_block_is_always_present() {
        let v = wxctl_schema::explain_kind("agent").unwrap();
        assert!(v.authoring.envelope.contains("kind") && v.authoring.envelope.contains("ref_name"));
        assert!(v.authoring.reference_syntax.contains("${"));
    }
}
