use clap::{Parser, Subcommand, ValueEnum};

use crate::output::resource_format::ResourceFormat;

#[derive(Clone, Debug, ValueEnum)]
pub enum OutputFormat {
    Json,
}

/// Declarative resource management
#[derive(Parser)]
#[command(
    name = "wxctl",
    version,
    subcommand_help_heading = "Main commands",
    after_help = "\
Other commands:
  compose    LLM compose pipeline (identify, paths, prompt, scaffold)
  profile    Manage configuration profiles

Workflow: init → validate → plan → apply → test → destroy
Machine output: --output json on validate, plan, apply, test, destroy, resources, explain
Authoring:      wxctl explain <kind> — fields, deps, ${kind.ref} syntax; wxctl explain — the config model
Exit codes:     0 success · 1 error · 2 usage
Docs:     https://github.com/randyphoa/wxctl"
)]
pub struct Cli {
    /// Configuration profile name [default: default].
    #[arg(short, long, value_name = "NAME", global = true)]
    pub profile: Option<String>,

    /// Path to a custom profile configuration file.
    #[arg(long, value_name = "PATH", global = true)]
    pub profile_path: Option<String>,

    /// Capture full-fidelity run records: redacted bodies for all exchanges,
    /// debug/trace internals, hook payload diffs, and `src` on every event.
    /// Also settable via `WXCTL_FULL_TRACE=1`.
    #[arg(long, global = true)]
    pub full_trace: bool,

    /// Skip the background update check for this run.
    /// Also settable via `WXCTL_NO_UPDATE_CHECK=1` or `DO_NOT_TRACK=1`.
    #[arg(long, global = true)]
    pub no_update_check: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Set up a profile with service URLs and auth settings.
    #[command(
        display_order = 1,
        after_help = "\
Examples:
  wxctl init                    Scaffold profiles.yaml for all services
  wxctl init -f config.yaml     Scaffold only the services used by config
  wxctl init -p prod --force    Re-scaffold the 'prod' profile in place
  wxctl init --edit             Scaffold, open $EDITOR, then validate

Next: fill in credentials, then run `wxctl profile validate`.
Docs: https://github.com/randyphoa/wxctl"
    )]
    Init {
        /// Config file(s) to scan for required services. If omitted, scaffolds all services.
        #[arg(short = 'f', long = "filename", value_name = "FILE")]
        config: Vec<String>,
        /// Open $VISUAL/$EDITOR on the scaffold, then run the profile's live validation checks.
        #[arg(long)]
        edit: bool,
        /// Overwrite the target profile with a fresh scaffold (other profiles and preferences are preserved).
        #[arg(long)]
        force: bool,
    },
    /// Validate configuration files against schemas.
    #[command(
        display_order = 2,
        after_help = "\
Examples:
  wxctl validate -f config.yaml                              Check configuration is valid
  wxctl validate -f config.yaml --fix-prompt                 Output LLM fix prompt for errors
  wxctl validate -f config.yaml --fix-prompt prompt.md       Include original prompt for retry
  wxctl validate -f config.yaml --output json                Output structured JSON errors"
    )]
    Validate {
        /// Config file(s), directory, or '-' for stdin. Can be specified multiple times.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// On validation failure, output an LLM fix prompt instead of the normal summary.
        /// Optionally pass the path to the original generation prompt to produce a
        /// Guardrails-style retry prompt (original prompt + failed output + errors).
        #[arg(long, value_name = "ORIGINAL_PROMPT", num_args = 0..=1, default_missing_value = "")]
        fix_prompt: Option<String>,
        /// Output format for structured output.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Skip post-validation checks (e.g. source_path existence) for pre-scaffold validation.
        #[arg(long)]
        skip_post_validate: bool,
    },
    /// Preview changes without applying them.
    #[command(display_order = 3)]
    Plan {
        /// Config file(s), directory, or '-' for stdin. Can be specified multiple times.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// Output format for structured output.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// Apply configuration and provision resources.
    #[command(
        display_order = 4,
        after_help = "\
Examples:
  wxctl apply -f config.yaml                 Provision resources
  wxctl apply -f agents.yaml -f tools.yaml   Merge multiple files
  wxctl apply -f ./configs/                  All YAML in a directory
  cat config.yaml | wxctl apply -f -         Read from stdin"
    )]
    Apply {
        /// Config file(s), directory, or '-' for stdin. Can be specified multiple times.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// Output format for structured output.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// Run tests against deployed resources.
    #[command(display_order = 5)]
    Test {
        /// Config file(s), directory, or '-' for stdin. Can be specified multiple times.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// Output format for structured output.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// List the resource kinds wxctl supports.
    #[command(
        display_order = 7,
        after_help = "\
Examples:
  wxctl resources                                List all supported kinds
  wxctl resources --service watsonx_data         Filter to one service
  wxctl resources --deployment software          Kinds available on Software
  wxctl resources -o json                        Machine-readable output
  wxctl resources -o markdown                    Markdown table (coverage docs)"
    )]
    Resources {
        /// Show only kinds belonging to this service (e.g. watsonx_data).
        #[arg(long, value_name = "NAME")]
        service: Option<String>,
        /// Show only kinds available on this deployment.
        #[arg(long, value_name = "DEPLOYMENT", value_parser = ["saas", "software"])]
        deployment: Option<String>,
        /// Output format.
        #[arg(short = 'o', long = "output", value_enum, default_value_t = ResourceFormat::Table)]
        output: ResourceFormat,
    },
    /// Show one resource kind's fields, dependencies, and endpoints.
    #[command(
        display_order = 8,
        after_help = "\
Examples:
  wxctl explain                           The config model + all resource kinds
  wxctl explain presto_engine             Fields + dependencies for a kind
  wxctl explain presto_engine -o json     Full descriptor as JSON
  wxctl explain tool -o yaml              Full descriptor as YAML"
    )]
    Explain {
        /// Resource kind to describe (e.g. presto_engine, tool, agent). Omit to show the config model and the full kind list.
        #[arg(value_name = "KIND")]
        kind: Option<String>,
        /// Output format.
        #[arg(short = 'o', long = "output", value_enum, default_value_t = ResourceFormat::Table)]
        output: ResourceFormat,
    },
    /// Download, verify, and install the latest wxctl release.
    #[command(
        display_order = 11,
        after_help = "\
Examples:
  wxctl update           Prompt, then download + verify + self-replace
  wxctl update --yes     Update without the confirmation prompt
  wxctl update --notes   Show release notes for newer versions; do not install
  wxctl update --force   Reinstall even when already on the latest version"
    )]
    Update {
        /// Update without the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Print release notes for newer versions and exit (no download/install).
        #[arg(long)]
        notes: bool,
        /// Reinstall even when already up to date (re-download + verify + self-replace the current version).
        #[arg(long)]
        force: bool,
    },
    /// List run records, or show one in detail.
    #[command(
        hide = true,
        display_order = 9,
        after_help = "\
Examples:
  wxctl runs list              All run records, newest first
  wxctl runs show <run_id>     Manifest + error index for one run
  wxctl runs show <run_id> --full   Also dump the raw event log"
    )]
    Runs {
        #[command(subcommand)]
        command: RunsCommands,
    },
    /// Diagnose a failed run: error code, failing exchange, fix, triage class.
    #[command(
        hide = true,
        display_order = 10,
        after_help = "\
Examples:
  wxctl debug                  Diagnose the latest failed run
  wxctl debug <run_id>         Diagnose a specific run
  wxctl debug -o json          Machine-readable bundle"
    )]
    Debug {
        /// Run id to diagnose. Defaults to the most recent failed/aborted run.
        #[arg(value_name = "RUN_ID")]
        run_id: Option<String>,
        /// Output format for the diagnosis bundle.
        #[arg(short = 'o', long = "output", value_enum)]
        output: Option<OutputFormat>,
    },
    /// Destroy all resources defined in configuration.
    #[command(display_order = 6)]
    Destroy {
        /// Config file(s), directory, or '-' for stdin. Can be specified multiple times.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// Output format for structured output.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// LLM compose pipeline: identify → paths → prompt → scaffold.
    #[command(hide = true)]
    Compose {
        #[command(subcommand)]
        command: ComposeCommands,
    },
    /// Manage configuration profiles.
    #[command(hide = true)]
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
    /// Run an MCP (Model Context Protocol) server exposing wxctl as tools.
    #[command(hide = true)]
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}

/// Deployment flavor for `compose paths` bridge activation.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ComposeDeployment {
    Saas,
    Software,
}

impl ComposeDeployment {
    /// Concrete deployment string passed to the resolver. `software` maps to a
    /// representative version (flavor-level constraints match any software version).
    pub fn as_deployment_str(self) -> &'static str {
        match self {
            ComposeDeployment::Saas => "saas",
            ComposeDeployment::Software => "software-5.3.0",
        }
    }
}

#[derive(Subcommand)]
pub enum ComposeCommands {
    /// Pass 1: assemble a resource-identification prompt from a use case.
    Identify {
        /// Natural-language description or path to a text file.
        #[arg(long, value_name = "TEXT_OR_FILE")]
        input: String,
        /// Write the prompt to a file instead of stdout.
        #[arg(short, long, value_name = "FILE")]
        output: Option<String>,
    },
    /// Pass 2: resolve dependencies and enumerate the recommended deployment path.
    Paths {
        /// Config file(s), directory, or '-' for stdin. Repeatable.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: Vec<String>,
        /// Target deployment for bridge activation.
        #[arg(long, value_enum, default_value_t = ComposeDeployment::Saas)]
        deployment: ComposeDeployment,
        /// Write resolved paths to a file instead of stdout.
        #[arg(short, long, value_name = "FILE")]
        output: Option<String>,
    },
    /// Assemble a config / implementation / test prompt.
    Prompt {
        /// Natural-language description or path to a text file.
        #[arg(long, value_name = "TEXT_OR_FILE")]
        input: Option<String>,
        /// Resolved paths YAML for config-prompt assembly.
        #[arg(long, value_name = "FILE")]
        paths: Option<String>,
        /// Directory to scan for existing resources (e.g. knowledge base documents).
        #[arg(long, value_name = "DIR")]
        resources_dir: Option<String>,
        /// Scaffold directory for implementation-prompt assembly.
        #[arg(long, value_name = "DIR", conflicts_with_all = ["paths", "test_config"])]
        scaffold_dir: Option<String>,
        /// Config file (implementation mode): join tool `ref_name` -> `description` into the prompt.
        #[arg(short = 'f', long = "filename", value_name = "FILE")]
        config: Option<String>,
        /// Config file for test-generation prompt assembly.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["paths", "scaffold_dir", "resources_dir"])]
        test_config: Option<String>,
        /// Write the prompt to a file instead of stdout.
        #[arg(short, long, value_name = "FILE")]
        output: Option<String>,
    },
    /// Materialize the source files a config references.
    Scaffold {
        /// Config file containing resource definitions.
        #[arg(short = 'f', long = "filename", required = true, value_name = "FILE")]
        config: String,
        /// Override base directory for source files.
        #[arg(short, long, value_name = "DIR")]
        output_dir: Option<String>,
        /// YAML file with LLM-generated implementations to apply.
        #[arg(long, value_name = "FILE")]
        apply_implementations: Option<String>,
        /// Print the manifest of files that would be written; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// Serve a stdio MCP server for the selected profile.
    Serve {
        /// Expose only the read-only tools (discovery, validate, plan, runs, and compose authoring); do not register the mutating apply/destroy/test/scaffold tools. Recommended for unattended agents — mutation is impossible by construction.
        #[arg(long)]
        read_only: bool,
    },
}

#[derive(Subcommand)]
pub enum RunsCommands {
    /// List all run records, newest first.
    List,
    /// Show one run's manifest, error index, and (with `--full`) raw events.
    Show {
        /// Run id (see `wxctl runs list`).
        #[arg(value_name = "RUN_ID")]
        run_id: String,
        /// Also dump the raw `events.jsonl`.
        #[arg(long)]
        full: bool,
    },
}

#[derive(Subcommand)]
pub enum ProfileCommands {
    /// List all configured profiles.
    List,
    /// Show details of a profile.
    Show {
        /// Profile name. Defaults to the active profile.
        name: Option<String>,
    },
    /// Set the active profile.
    Use {
        /// Profile name to activate.
        name: String,
    },
    /// Validate a profile's configuration and connectivity.
    Validate {
        /// Profile name. Defaults to the active profile.
        name: Option<String>,
        /// Skip connectivity checks.
        #[arg(long)]
        no_connect: bool,
    },
}
