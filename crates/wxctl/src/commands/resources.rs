//! `wxctl resources` — list every registered resource kind with its service,
//! deployment support, and description. Read-only: no profile/creds/network.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use wxctl_core::registry::ResourceDescriptor;

use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{Color, Role};
use crate::output::resource_format::{Column, ResourceFormat, print_markdown_table, render};

/// One row of the resource listing.
#[derive(Serialize)]
pub(crate) struct ResourceRow {
    pub(crate) kind: String,
    /// Friendly IBM product name for `service` (display label; `service` is the join key).
    pub(crate) product: String,
    pub(crate) service: String,
    pub(crate) deployment: String,
    /// API path the create call hits — the join key against an API-spec surface.
    pub(crate) endpoint: String,
    pub(crate) description: String,
}

/// Friendly IBM product name for a service identifier. Single source of truth
/// for the labels the coverage docs group by; unknown services map to themselves.
/// Shared with `explain` so both commands render the same product labels.
pub(crate) fn product_name(service: &str) -> &str {
    match service {
        "watsonx_ai" => "watsonx.ai",
        "watsonx_data" => "watsonx.data",
        "watsonx_orchestrate" => "watsonx Orchestrate",
        "common_core" => "Data & AI Common Core",
        "openscale" => "OpenScale",
        "factsheets" => "AI Factsheets",
        "concert" => "IBM Concert",
        "concert_workflows" => "IBM Concert Workflows",
        "instana" => "IBM Instana",
        "planning_analytics" => "IBM Planning Analytics",
        "pa_workspace" => "IBM Planning Analytics Workspace",
        "cloud_object_storage" => "Cloud Object Storage",
        "local" => "Local",
        other => other,
    }
}

/// Map each kind to its create endpoint (the POST path), parsed once from the
/// shipped schemas. Read-only: no profile / network, same as the catalog.
fn create_endpoints() -> Result<HashMap<String, String>> {
    let schemas = wxctl_providers::load_all_schemas()?;
    Ok(schemas.iter().filter_map(|s| ResourceDescriptor::from_schema(s).ok().map(|d| (d.kind, d.endpoints.create))).collect())
}

/// Dispatch `wxctl resources`.
pub fn execute(service: Option<&str>, deployment: Option<&str>, format: ResourceFormat) -> Result<()> {
    let rows = build_rows(service, deployment)?;
    render(&rows, format, |fmt| render_rows_text(&rows, fmt))
}

/// Build the sorted resource-catalog rows (kind, product, service, deployment,
/// create endpoint, description). Read-only: parses the shipped schemas, no
/// profile / network. Shared with `explain` (no kind), which renders the same
/// rows after its authoring block.
pub(crate) fn build_rows(service: Option<&str>, deployment: Option<&str>) -> Result<Vec<ResourceRow>> {
    let endpoints = create_endpoints()?;

    let mut rows: Vec<ResourceRow> = wxctl_schema::list_kinds(service, deployment)
        .into_iter()
        .map(|k| ResourceRow { kind: k.kind.clone(), product: product_name(&k.service).to_string(), service: k.service.clone(), deployment: k.deployment_support.join(", "), endpoint: endpoints.get(k.kind.as_str()).cloned().unwrap_or_default(), description: k.summary.clone() })
        .collect();

    rows.sort_by(|a, b| a.product.cmp(&b.product).then_with(|| a.kind.cmp(&b.kind)));
    Ok(rows)
}

/// Render pre-built rows in a text format: the grouped panel view for `Table`, a
/// GitHub-flavored table for `Markdown`. JSON / YAML are handled by the `render`
/// dispatcher over the rows.
pub(crate) fn render_rows_text(rows: &[ResourceRow], format: ResourceFormat) {
    match format {
        ResourceFormat::Markdown => {
            let columns = vec![
                Column { header: "KIND", values: rows.iter().map(|r| r.kind.clone()).collect() },
                Column { header: "PRODUCT", values: rows.iter().map(|r| r.product.clone()).collect() },
                Column { header: "SERVICE", values: rows.iter().map(|r| r.service.clone()).collect() },
                Column { header: "DEPLOYMENT", values: rows.iter().map(|r| if r.deployment.is_empty() { "-".to_string() } else { r.deployment.clone() }).collect() },
                Column { header: "ENDPOINT", values: rows.iter().map(|r| if r.endpoint.is_empty() { "-".to_string() } else { r.endpoint.clone() }).collect() },
                Column { header: "DESCRIPTION", values: rows.iter().map(|r| r.description.clone()).collect() },
            ];
            print_markdown_table(&columns);
        }
        _ => {
            for line in print_grouped(rows, &Panel::resolve(None)) {
                println!("{line}");
            }
        }
    }
}

/// Render the catalog as panel sections, one per IBM product, each listing its
/// kinds with deployment tags and the full, hanging-indent-wrapped description.
/// Colors and width come from the injected `Panel`. Returns one `String` per
/// line; the caller prints.
pub(crate) fn print_grouped(rows: &[ResourceRow], panel: &Panel) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if rows.is_empty() {
        out.push(panel.paint(Role::Meta, "No resource kinds match the given filters."));
        return out;
    }

    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(4);
    let deploy_w = rows.iter().map(|r| r.deployment.len().max(1)).max().unwrap_or(1);

    let product_count = rows.iter().map(|r| r.product.as_str()).collect::<std::collections::BTreeSet<_>>().len();
    out.push(String::new());
    out.push(format!("  {}   {}", panel.paint(Role::Heading, "resource catalog"), panel.paint(Role::Meta, &format!("{} kind{} {} {} product{}", rows.len(), if rows.len() == 1 { "" } else { "s" }, panel.g("dot"), product_count, if product_count == 1 { "" } else { "s" }))));

    let mut i = 0;
    while i < rows.len() {
        let start = i;
        while i < rows.len() && rows[i].product == rows[start].product {
            i += 1;
        }
        let group = &rows[start..i];
        let head = &group[0];

        out.push(String::new());
        let hint = format!("{} {} {} kind{}", head.service, panel.g("dot"), group.len(), if group.len() == 1 { "" } else { "s" });
        out.push(panel.section(&head.product, Some(&hint)));

        for r in group {
            // Marker row: kind + deployment tags, then the description.
            let desc = if r.description.trim().is_empty() { panel.g("emdash").to_string() } else { r.description.clone() };
            let lead = format!("    {}   {}   ", panel.paint(Role::Active, &format!("{:<kind_w$}", r.kind)), deploy_tags(panel, &r.deployment, deploy_w));
            // visible indent = 4 + kind_w + 3 + deploy_w + 3
            let indent = 4 + kind_w + 3 + deploy_w + 3;
            let wrapped = panel.wrap_hanging(&desc, indent);
            // First line: lead + first wrapped chunk (strip its leading pad — lead already positions it).
            out.push(format!("{}{}", lead, panel.paint(Role::Meta, wrapped[0].trim_start())));
            for cont in &wrapped[1..] {
                out.push(panel.paint(Role::Meta, cont));
            }
        }
    }
    out.push(String::new());
    out
}

/// Color each deployment token (saas=green, software=amber) and pad the cell to
/// `width` *visible* columns so the following text stays aligned despite the
/// ANSI escapes `paint` injects. Pass `width = 0` for an unpadded label (used by
/// `explain`'s header). Shared by `resources` and `explain`.
pub(crate) fn deploy_tags(panel: &Panel, deployment: &str, width: usize) -> String {
    if deployment.is_empty() {
        return format!("{}{}", panel.paint(Role::Meta, panel.g("emdash")), " ".repeat(width.saturating_sub(1)));
    }
    let painted = deployment
        .split(", ")
        .map(|t| match t {
            "saas" => panel.paint_color(Color::Green, t),
            "software" => panel.paint_color(Color::Yellow, t),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}{}", painted, " ".repeat(width.saturating_sub(deployment.len())))
}
