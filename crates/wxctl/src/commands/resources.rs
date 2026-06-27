//! `wxctl resources` — list every registered resource kind with its service,
//! deployment support, and description. Read-only: no profile/creds/network.

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use wxctl_core::registry::ResourceDescriptor;

use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{Color, Role, Theme, vt_enabled};
use crate::output::resource_format::{Column, ResourceFormat, print_markdown_table, render};

/// One row of the resource listing.
#[derive(Serialize)]
struct ResourceRow {
    kind: String,
    /// Friendly IBM product name for `service` (display label; `service` is the join key).
    product: String,
    service: String,
    deployment: String,
    /// API path the create call hits — the join key against an API-spec surface.
    endpoint: String,
    description: String,
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
    let endpoints = create_endpoints()?;

    let mut rows: Vec<ResourceRow> = wxctl_schema::list_kinds(service, deployment)
        .into_iter()
        .map(|k| ResourceRow { kind: k.kind.clone(), product: product_name(&k.service).to_string(), service: k.service.clone(), deployment: k.deployment_support.join(", "), endpoint: endpoints.get(k.kind.as_str()).cloned().unwrap_or_default(), description: k.summary.clone() })
        .collect();

    rows.sort_by(|a, b| a.product.cmp(&b.product).then_with(|| a.kind.cmp(&b.kind)));

    render(&rows, format, |fmt| match fmt {
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
            let theme = Theme::resolve(None);
            let width = Panel::resolve_width();
            let glyphs = GlyphSet::resolve(vt_enabled() && !theme.is_plain());
            print_grouped(&rows, &Panel::new(theme, width, glyphs));
        }
    })
}

/// Render the catalog as panel sections, one per IBM product, each listing its
/// kinds with deployment tags and a hanging-indent-wrapped description. Colors
/// and width come from the injected `Panel` (deterministic for snapshots).
fn print_grouped(rows: &[ResourceRow], panel: &Panel) {
    if rows.is_empty() {
        println!("{}", panel.paint(Role::Meta, "No resource kinds match the given filters."));
        return;
    }

    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(4);
    let deploy_w = rows.iter().map(|r| r.deployment.len().max(1)).max().unwrap_or(1);

    let product_count = rows.iter().map(|r| r.product.as_str()).collect::<std::collections::BTreeSet<_>>().len();
    println!();
    println!("  {}   {}", panel.paint(Role::Heading, "watsonx resource catalog"), panel.paint(Role::Meta, &format!("{} kind{} {} {} product{}", rows.len(), if rows.len() == 1 { "" } else { "s" }, panel.g("dot"), product_count, if product_count == 1 { "" } else { "s" })));

    let mut i = 0;
    while i < rows.len() {
        let start = i;
        while i < rows.len() && rows[i].product == rows[start].product {
            i += 1;
        }
        let group = &rows[start..i];
        let head = &group[0];

        println!();
        let hint = format!("{} {} {} kind{}", head.service, panel.g("dot"), group.len(), if group.len() == 1 { "" } else { "s" });
        println!("{}", panel.section(&head.product, Some(&hint)));

        for r in group {
            // Marker row: kind + deployment tags, then a hanging-indent-wrapped description.
            let desc = if r.description.trim().is_empty() { panel.g("emdash").to_string() } else { r.description.clone() };
            let lead = format!("    {}   {}   ", panel.paint(Role::Active, &format!("{:<kind_w$}", r.kind)), deploy_tags(panel, &r.deployment, deploy_w));
            // visible indent = 4 + kind_w + 3 + deploy_w + 3
            let indent = 4 + kind_w + 3 + deploy_w + 3;
            let wrapped = panel.wrap_hanging(&desc, indent);
            // First line: lead + first wrapped chunk (strip its leading pad — lead already positions it).
            let first = wrapped[0].trim_start();
            println!("{}{}", lead, panel.paint(Role::Meta, first));
            for cont in &wrapped[1..] {
                println!("{}", panel.paint(Role::Meta, cont));
            }
        }
    }
    println!();
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
