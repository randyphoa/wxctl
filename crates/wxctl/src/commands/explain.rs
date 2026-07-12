use crate::commands::resources::{build_rows, deploy_tags, product_name, render_rows_text};
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{Color, Role};
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
            render(&view, format, |_| {
                for line in render_table(&Panel::resolve(None), &view) {
                    println!("{line}");
                }
            })
        }
        None => {
            let overview = AuthoringOverview { authoring: wxctl_schema::authoring_overview(), kinds: wxctl_schema::list_kinds(None, None) };
            match format {
                ResourceFormat::Json => print_json(&overview)?,
                ResourceFormat::Yaml => print_yaml(&overview)?,
                ResourceFormat::Table | ResourceFormat::Markdown => {
                    let rows = build_rows(None, None)?;
                    if matches!(format, ResourceFormat::Table) {
                        let panel = Panel::resolve(None);
                        for line in render_usage(&panel).into_iter().chain(render_authoring(&panel, &overview.authoring)) {
                            println!("{line}");
                        }
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
/// section bars, dim rules). Verbs render neutral (the uppercase text carries the
/// method); semantic color is reserved for outcomes. Width + colors come from the
/// injected `Panel` (so snapshots build a fixed one). Returns one `String` per
/// line; the caller prints.
pub(crate) fn render_table(panel: &Panel, view: &ExplainView) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // ── header: kind · product · deployment, then the id field ──
    let deployment = deployment_support(&view.kind).join(", ");
    out.push(String::new());
    out.push(format!("  {}   {} {} {}", panel.paint(Role::Heading, &view.kind), panel.paint(Role::Meta, product_name(&view.service)), panel.g("dot"), deploy_tags(panel, &deployment, 0)));
    out.push(format!("  {}", panel.paint(Role::Meta, &format!("id field {} {}", panel.g("dot"), view.id_field))));

    // ── endpoints, lifecycle order, verbs neutral ──
    out.extend(section(panel, "Endpoints", None));
    let mut endpoints: Vec<(&str, &str, &str)> = vec![("create", "POST", &view.endpoints.create), ("get", "GET", &view.endpoints.get)];
    if let Some(list) = &view.endpoints.list {
        endpoints.push(("list", "GET", list));
    }
    if let Some(update) = &view.endpoints.update {
        endpoints.push(("update", view.endpoints.update_method.as_deref().unwrap_or("PATCH"), update));
    }
    endpoints.push(("delete", "DELETE", &view.endpoints.delete));
    for (label, verb, path) in endpoints {
        out.push(format!("    {}   {}   {}", panel.paint(Role::Meta, &format!("{:<6}", label)), panel.paint(Role::Meta, &format!("{:<6}", verb)), path));
    }

    // ── fields, required first, with status + locked flags ──
    out.extend(section(panel, "Fields", Some("required first")));
    let name_w = view.fields.iter().map(|f| f.name.len()).max().unwrap_or(4);
    let type_w = view.fields.iter().map(|f| type_label(f).len()).max().unwrap_or(4);
    let mut fields: Vec<&ExplainField> = view.fields.iter().collect();
    fields.sort_by_key(|f| field_rank(f));
    for f in &fields {
        let (status, color) = field_status(f);
        let locked = if f.immutable { panel.paint(Role::Meta, "locked") } else { " ".repeat(6) };
        out.push(format!("    {:<name_w$}   {}   {}   {}   {}", f.name, panel.paint(Role::Meta, &format!("{:<type_w$}", type_label(f))), panel.paint_color(color, &format!("{:<8}", status)), locked, panel.paint(Role::Meta, &f.location)));
    }

    // ── dependencies as field ──▶ target arrows ──
    out.extend(section(panel, "Dependencies", None));
    if view.dependencies.is_empty() {
        out.push(format!("    {}", panel.paint(Role::Meta, "(none)")));
    } else {
        let dep_w = view.dependencies.iter().map(|d| d.field.len()).max().unwrap_or(4);
        for d in &view.dependencies {
            let req = if d.required { panel.paint_color(Color::BoldWhite, "required") } else { panel.paint(Role::Meta, "optional") };
            out.push(format!("    {:<dep_w$}  {}  {}   {}", d.field, panel.paint(Role::Meta, panel.g("arrow")), panel.paint(Role::Active, &format!("${{{}.<ref_name>}}", d.target_kind)), req));
        }
    }

    // ── prompt notes (optional) ──
    if !view.prompt_notes.is_empty() {
        out.extend(section(panel, "Notes", None));
        for note in &view.prompt_notes {
            out.push(format!("    {} {}", panel.paint(Role::Meta, panel.g("bullet")), panel.paint(Role::Meta, note)));
        }
    }

    // ── authoring conventions ──
    out.extend(render_authoring(panel, &view.authoring));
    out.push(String::new());
    out
}

/// Render the `▌ Usage` section shown by the bare `explain` (no kind): a high-level
/// pointer at the per-kind detail view and the discover → learn → author → check
/// flow, so `explain` on its own teaches how to use itself rather than only
/// reprinting the catalog. Returns one `String` per line; the caller prints.
fn render_usage(panel: &Panel) -> Vec<String> {
    let arrow = panel.g("arrow");
    let mut out = section(panel, "Usage", None);
    out.push(format!("    {}   {}", panel.paint(Role::Meta, &format!("{:<14}", "explain <kind>")), panel.paint(Role::Meta, "endpoints, fields, and dependencies to author one kind")));
    out.push(format!("    {}   {}", panel.paint(Role::Meta, &format!("{:<14}", "flow")), panel.paint(Role::Meta, &format!("resources {arrow} explain <kind> {arrow} author config {arrow} validate"))));
    out
}

/// Render the `▌ Authoring` section: the envelope / `ref_name` / reference-syntax
/// conventions. Shared by the per-kind view and the no-kind config-model overview.
/// Returns one `String` per line; the caller prints.
fn render_authoring(panel: &Panel, authoring: &Authoring) -> Vec<String> {
    let mut out = section(panel, "Authoring", None);
    out.push(format!("    {} {}", panel.paint(Role::Meta, "envelope  "), panel.paint(Role::Meta, authoring.envelope)));
    out.push(format!("    {} {}", panel.paint(Role::Meta, "ref_name  "), panel.paint(Role::Meta, authoring.ref_name)));
    out.push(format!("    {} {}", panel.paint(Role::Meta, "reference "), panel.paint(Role::Meta, authoring.reference_syntax)));
    out
}

/// A `▌ Title   (hint)` section header as a blank spacer line + the header line.
fn section(panel: &Panel, title: &str, hint: Option<&str>) -> Vec<String> {
    vec![String::new(), panel.section(title, hint)]
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

/// Status label + color: computed fields are derived (dim), required inputs
/// bold-neutral (`BoldWhite`), optional dim. Green/red/amber stay reserved for
/// outcomes, not descriptors.
fn field_status(f: &ExplainField) -> (&'static str, Color) {
    if f.location == "Computed" {
        ("computed", Color::Dim)
    } else if f.required {
        ("required", Color::BoldWhite)
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
