//! AnyCLI — turn any website into structured CLI output.

use std::io;
use std::process;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

use anycli::adapter::Command;
use anycli::{Hub, OutputFormat, Pipeline, Registry, set_color_enabled, set_timeout_secs, set_verbose};

const META_COMMANDS: &[&str] = &[
    "run",
    "list",
    "info",
    "search",
    "install",
    "update",
    "uninstall",
    "validate",
    "completions",
    "new",
    "doctor",
    "cat",
    "eject",
    "help",
];

#[derive(Parser)]
#[command(name = "anycli", version, about = "Turn any website into structured CLI output")]
struct Cli {
    /// Print request URLs (also set ANYCLI_VERBOSE=1).
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Request timeout in seconds (overrides the 30s default).
    #[arg(long, global = true)]
    timeout: Option<u64>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an adapter command to extract data from a website.
    Run {
        /// Adapter name (e.g., "hackernews", "bilibili").
        adapter: String,
        /// Command name (e.g., "top", "search", "help"). Shows help if omitted.
        command: Option<String>,
        /// Parameters as key=value pairs (e.g., limit=10 query="rust").
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        params: Vec<String>,
        /// Output format: table, json, jsonc, md, yaml, csv, plain.
        #[arg(long, short, default_value = "table")]
        format: String,
        /// Comma-separated field names to include (and their order).
        #[arg(long)]
        fields: Option<String>,
        /// Disable ANSI colors in table output.
        #[arg(long)]
        no_color: bool,
        /// Sort rows by this field (numeric when possible).
        #[arg(long)]
        sort: Option<String>,
        /// Reverse sort order (use with --sort).
        #[arg(long)]
        reverse: bool,
        /// Compact JSON (same as `--format jsonc`).
        #[arg(long)]
        compact: bool,
    },
    /// List all available adapters.
    List {
        /// Output format: table or json.
        #[arg(long, short, default_value = "table")]
        format: String,
        /// Filter by name, alias, tag, or description substring.
        #[arg(long, short)]
        tag: Option<String>,
    },
    /// Show details of a specific adapter.
    Info {
        /// Adapter name.
        adapter: String,
    },
    /// Search adapters in the community hub.
    Search {
        /// Search query.
        query: String,
    },
    /// Install an adapter from the community hub.
    Install {
        /// Adapter name to install.
        name: String,
    },
    /// Update all installed adapters from the hub.
    Update,
    /// Uninstall a user-installed adapter.
    Uninstall {
        /// Adapter name to remove from ~/.anycli/adapters/.
        name: String,
    },
    /// Validate an adapter YAML file.
    Validate {
        /// Path to a YAML adapter file.
        path: String,
    },
    /// Generate shell completion script.
    Completions {
        /// Target shell: bash, zsh, fish, powershell, elvish.
        shell: String,
    },
    /// Scaffold a custom adapter YAML in ~/.anycli/adapters/.
    New {
        /// Adapter name (letters, digits, hyphen).
        name: String,
        /// Base URL for the adapter.
        #[arg(long)]
        url: Option<String>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Check installation: adapters, user dir, browser backends.
    Doctor,
    /// Print the YAML source of an adapter.
    Cat {
        /// Adapter name.
        adapter: String,
    },
    /// Copy a built-in adapter into ~/.anycli/adapters/ for editing.
    Eject {
        /// Adapter name.
        adapter: String,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().collect();

    // `anycli hackernews top` is the same as `anycli run hackernews top`.
    // Insert `run` before the first non-flag that isn't a meta-command, so
    // `anycli -v wikipedia search rust` still works.
    if let Some(i) = args.iter().skip(1).position(|a| !a.starts_with('-')) {
        let idx = i + 1;
        if !META_COMMANDS.contains(&args[idx].as_str()) {
            args.insert(idx, "run".to_owned());
        }
    }

    // Intercept `anycli run <adapter> --help` before clap parses.
    if args.len() >= 3 && args[1] == "run" {
        let has_help = args[2..].iter().any(|a| a == "--help" || a == "-h");
        if has_help {
            let registry = Registry::load()?;
            let adapter_name = &args[2];
            if let Ok(adapter) = registry.find(adapter_name) {
                let cmd_name = args[3..]
                    .iter()
                    .find(|a| *a != "--help" && *a != "-h" && !a.starts_with('-'));
                if let Some(cmd) = cmd_name {
                    print_command_help(adapter, cmd)?;
                } else {
                    print_adapter_help(adapter, None);
                }
                return Ok(());
            }
        }
    }

    let cli = Cli::parse_from(args);
    if cli.verbose {
        set_verbose(true);
    }
    if let Some(secs) = cli.timeout {
        set_timeout_secs(secs);
    }
    let registry = Registry::load()?;

    match cli.command {
        Commands::List { format, tag } => {
            let adapters = if let Some(ref tag) = tag {
                registry.search(tag)
            } else {
                registry.list()
            };
            print_adapter_list(&adapters, &format)?;
        }

        Commands::Info { adapter: name } => {
            let adapter = registry.find(&name)?;
            println!("Name:        {}", adapter.name);
            println!("Description: {}", adapter.description);
            println!("Base URL:    {}", adapter.base_url);
            if !adapter.version.is_empty() {
                println!("Version:     {}", adapter.version);
            }
            if !adapter.aliases.is_empty() {
                println!("Aliases:     {}", adapter.aliases.join(", "));
            }
            if !adapter.tags.is_empty() {
                println!("Tags:        {}", adapter.tags.join(", "));
            }
            println!("\nCommands:");
            for (cmd_name, cmd) in &adapter.commands {
                println!("  {cmd_name:<16} {}", cmd.description);
                for (param_name, param) in &cmd.params {
                    let req = if param.required { " (required)" } else { "" };
                    let desc = param.description.as_deref().unwrap_or("");
                    let default = param
                        .default
                        .as_ref()
                        .map(|d| format!(" [default: {d}]"))
                        .unwrap_or_default();
                    println!("    {param_name:<14} {desc}{default}{req}");
                }
            }
        }

        Commands::Run {
            adapter: name,
            command,
            params,
            format,
            fields,
            no_color,
            sort,
            reverse,
            compact,
        } => {
            let adapter = registry.find(&name)?;
            let (flags, raw_params) = strip_output_flags(&params);
            set_color_enabled(!no_color && !flags.no_color);

            let command = match command {
                None => {
                    print_adapter_help(adapter, None);
                    return Ok(());
                }
                Some(c) => c,
            };

            if command == "help" || command == "--help" || command == "-h" {
                print_adapter_help(adapter, raw_params.first().map(|s| s.as_str()));
                return Ok(());
            }

            if raw_params
                .iter()
                .any(|p| p == "--help" || p == "-h" || p == "help")
            {
                print_command_help(adapter, &command)?;
                return Ok(());
            }

            let cmd = adapter.command(&command).map(|(_, c)| c);
            let parsed = parse_params(&raw_params, cmd);
            let fmt_str = flags.format.as_deref().unwrap_or(&format);
            let mut fmt: OutputFormat = fmt_str.parse()?;
            if (compact || flags.compact) && fmt == OutputFormat::Json {
                fmt = OutputFormat::JsonCompact;
            }

            let param_refs: Vec<(&str, &str)> = parsed
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            let mut result = Pipeline::execute(adapter, &command, &param_refs).await?;

            let sort_field = flags.sort.as_ref().or(sort.as_ref());
            if let Some(field) = sort_field {
                result.sort_by(field, reverse || flags.reverse);
            }

            let fields = flags.fields.as_ref().or(fields.as_ref());
            if let Some(fields) = fields {
                let cols: Vec<String> = fields
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !cols.is_empty() {
                    result.project_fields(&cols);
                }
            }

            println!("{}", result.format(fmt)?);
        }

        Commands::Search { query } => {
            let local = registry.search(&query);
            let hub_results = match Hub::new() {
                Ok(hub) => hub.search(&query).await.unwrap_or_default(),
                Err(_) => Vec::new(),
            };

            if local.is_empty() && hub_results.is_empty() {
                println!("No adapters found for `{query}`");
            } else {
                println!("{:<22} {:<8} {}", "ADAPTER", "SOURCE", "DESCRIPTION");
                println!("{:<22} {:<8} {}", "-------", "------", "-----------");
                let mut seen = std::collections::HashSet::new();
                for adapter in &local {
                    seen.insert(adapter.name.as_str());
                    println!(
                        "{:<22} {:<8} {}",
                        adapter.name, "local", adapter.description
                    );
                }
                for entry in &hub_results {
                    if seen.contains(entry.name.as_str()) {
                        continue;
                    }
                    println!(
                        "{:<22} {:<8} {}",
                        entry.name, "hub", entry.description
                    );
                }
                if !hub_results.is_empty() {
                    println!("\nInstall from hub: anycli install <name>");
                }
            }
        }

        Commands::Install { name } => {
            let hub = Hub::new()?;
            let dir = anycli::hub::default_adapters_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            let path = hub.install(&name, &dir).await?;
            println!("Installed `{name}` to {}", path.display());
        }

        Commands::Update => {
            let hub = Hub::new()?;
            let dir = anycli::hub::default_adapters_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            let (updated, total) = hub.update(&dir).await?;
            println!("Updated {updated}/{total} adapters");
        }

        Commands::Uninstall { name } => {
            let dir = anycli::hub::default_adapters_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            let candidates = [
                dir.join(format!("{name}.yaml")),
                dir.join(format!("{name}.yml")),
                dir.join(format!("{}.yaml", name.replace('-', "_"))),
            ];
            let path = candidates.into_iter().find(|p| p.exists());
            match path {
                Some(path) => {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("failed to remove {}", path.display()))?;
                    println!("Uninstalled `{name}` from {}", path.display());
                }
                None => bail!("adapter `{name}` is not installed in {}", dir.display()),
            }
        }

        Commands::Validate { path } => {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {path}"))?;
            let adapter: anycli::Adapter = serde_yaml_ng::from_str(&content)
                .with_context(|| format!("invalid adapter YAML: {path}"))?;
            if adapter.name.is_empty() {
                bail!("adapter is missing a name");
            }
            if adapter.commands.is_empty() {
                bail!("adapter `{}` has no commands", adapter.name);
            }
            for (cmd_name, cmd) in &adapter.commands {
                if cmd.url.is_empty()
                    && cmd.evaluate.is_none()
                    && cmd.data.is_none()
                    && cmd.format != anycli::adapter::SourceFormat::Desktop
                    && cmd.format != anycli::adapter::SourceFormat::Static
                {
                    bail!("command `{cmd_name}` needs a url, evaluate script, or static data");
                }
                for (param_name, param) in &cmd.params {
                    if param.required && param.default.is_some() {
                        eprintln!(
                            "warning: `{cmd_name}.{param_name}` is required but also has a default"
                        );
                    }
                }
            }
            println!(
                "ok: {} ({} command{})",
                adapter.name,
                adapter.commands.len(),
                if adapter.commands.len() == 1 { "" } else { "s" }
            );
        }

        Commands::Completions { shell } => {
            let shell: Shell = shell
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown shell `{shell}`. supported: bash, zsh, fish, powershell, elvish"))?;
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "anycli", &mut io::stdout());
        }

        Commands::New { name, url, force } => {
            create_adapter_scaffold(&name, url.as_deref(), force)?;
        }

        Commands::Doctor => {
            run_doctor(&registry)?;
        }

        Commands::Cat { adapter: name } => {
            let yaml = registry.source_yaml(&name)?;
            print!("{yaml}");
            if !yaml.ends_with('\n') {
                println!();
            }
        }

        Commands::Eject { adapter: name, force } => {
            let adapter = registry.find(&name)?;
            let yaml = registry.source_yaml(&name)?;
            let dir = anycli::hub::default_adapters_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
            let dest = dir.join(format!("{}.yaml", adapter.name));
            if dest.exists() && !force {
                bail!("{} already exists (pass --force to overwrite)", dest.display());
            }
            std::fs::write(&dest, yaml)
                .with_context(|| format!("failed to write {}", dest.display()))?;
            println!("Wrote {}", dest.display());
            println!("User adapters override built-ins. Edit, then: anycli validate {}", dest.display());
        }
    }

    Ok(())
}

#[derive(Default)]
struct OutputFlags {
    format: Option<String>,
    fields: Option<String>,
    no_color: bool,
    sort: Option<String>,
    reverse: bool,
    compact: bool,
}

/// Pull output-related flags out of trailing adapter params.
fn strip_output_flags(params: &[String]) -> (OutputFlags, Vec<String>) {
    let mut flags = OutputFlags::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let p = &params[i];
        if p == "--no-color" {
            flags.no_color = true;
            i += 1;
            continue;
        }
        if p == "--verbose" || p == "-v" {
            set_verbose(true);
            i += 1;
            continue;
        }
        if p == "--reverse" {
            flags.reverse = true;
            i += 1;
            continue;
        }
        if p == "--compact" {
            flags.compact = true;
            i += 1;
            continue;
        }
        if p == "--timeout" {
            if let Some(val) = params.get(i + 1) {
                if let Ok(secs) = val.parse::<u64>() {
                    set_timeout_secs(secs);
                }
                i += 2;
                continue;
            }
        }
        if let Some(val) = p.strip_prefix("--timeout=") {
            if let Ok(secs) = val.parse::<u64>() {
                set_timeout_secs(secs);
            }
            i += 1;
            continue;
        }
        if p == "--sort" {
            if let Some(val) = params.get(i + 1) {
                flags.sort = Some(val.clone());
                i += 2;
                continue;
            }
        }
        if let Some(val) = p.strip_prefix("--sort=") {
            flags.sort = Some(val.to_string());
            i += 1;
            continue;
        }
        if p == "--format" || p == "-f" {
            if let Some(val) = params.get(i + 1) {
                flags.format = Some(val.clone());
                i += 2;
                continue;
            }
        }
        if let Some(val) = p.strip_prefix("--format=") {
            flags.format = Some(val.to_string());
            i += 1;
            continue;
        }
        if p == "--fields" {
            if let Some(val) = params.get(i + 1) {
                flags.fields = Some(val.clone());
                i += 2;
                continue;
            }
        }
        if let Some(val) = p.strip_prefix("--fields=") {
            flags.fields = Some(val.to_string());
            i += 1;
            continue;
        }
        rest.push(p.clone());
        i += 1;
    }
    (flags, rest)
}

/// Parse params from multiple formats:
/// - key=value
/// - --key value
/// - --key=value
/// - leftover positionals bound to `positional: true` params (or the single required param)
fn parse_params(params: &[String], cmd: Option<&Command>) -> Vec<(String, String)> {
    let mut parsed = Vec::new();
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < params.len() {
        let p = &params[i];
        if let Some(rest) = p.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                parsed.push((k.to_owned(), v.to_owned()));
            } else {
                let key = rest.to_owned();
                if i + 1 < params.len() && !params[i + 1].starts_with("--") && !params[i + 1].contains('=')
                {
                    i += 1;
                    parsed.push((key, params[i].clone()));
                } else {
                    parsed.push((key, "true".to_owned()));
                }
            }
        } else if let Some((k, v)) = p.split_once('=') {
            parsed.push((k.to_owned(), v.to_owned()));
        } else {
            positionals.push(p.clone());
        }
        i += 1;
    }

    if let Some(cmd) = cmd {
        let used: std::collections::HashSet<&str> =
            parsed.iter().map(|(k, _)| k.as_str()).collect();
        let mut names: Vec<String> = cmd
            .params
            .iter()
            .filter(|(n, p)| p.positional && !used.contains(n.as_str()))
            .map(|(n, _)| n.clone())
            .collect();
        if names.is_empty() && positionals.len() == 1 {
            names = cmd
                .params
                .iter()
                .filter(|(n, p)| p.required && !used.contains(n.as_str()))
                .map(|(n, _)| n.clone())
                .take(1)
                .collect();
        }
        for (i, val) in positionals.iter().enumerate() {
            if let Some(name) = names.get(i) {
                parsed.push((name.clone(), val.clone()));
            }
        }
    }

    parsed
}

fn print_adapter_list(adapters: &[&anycli::Adapter], format: &str) -> Result<()> {
    if format == "json" {
        let rows: Vec<serde_json::Value> = adapters
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.name,
                    "description": a.description,
                    "version": a.version,
                    "aliases": a.aliases,
                    "tags": a.tags,
                    "commands": a.commands.len(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("{:<22} {:<8} {}", "ADAPTER", "CMDS", "DESCRIPTION");
    println!("{:<22} {:<8} {}", "-------", "----", "-----------");
    for adapter in adapters {
        println!(
            "{:<22} {:<8} {}",
            adapter.name,
            adapter.commands.len(),
            adapter.description
        );
    }
    println!("\n{} adapters", adapters.len());
    Ok(())
}

fn create_adapter_scaffold(name: &str, url: Option<&str>, force: bool) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("adapter name must be alphanumeric with hyphens/underscores");
    }
    let dir = anycli::hub::default_adapters_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let dest = dir.join(format!("{name}.yaml"));
    if dest.exists() && !force {
        bail!(
            "{} already exists (pass --force to overwrite)",
            dest.display()
        );
    }

    let base = url.unwrap_or("https://api.example.com");
    let yaml = format!(
        r#"name: {name}
description: "{name} adapter"
base_url: "{base}"
version: "0.1.0"
tags: []

commands:
  search:
    description: "Search {name}"
    url: "/search?q={{query}}&limit={{limit}}"
    format: json
    selector: "data.items"
    fields:
      title:
        json_path: "title"
      url:
        json_path: "url"
        default: ""
    params:
      query:
        type: string
        required: true
        description: "Search query"
      limit:
        type: integer
        default: 10
        description: "Number of results"
"#
    );

    let adapter: anycli::Adapter = serde_yaml_ng::from_str(&yaml)
        .context("internal error: generated adapter YAML is invalid")?;
    let _ = adapter;
    std::fs::write(&dest, yaml).with_context(|| format!("failed to write {}", dest.display()))?;
    println!("Wrote {}", dest.display());
    println!("Edit the file, then: anycli validate {}", dest.display());
    println!("Try it: anycli {name} search query=hello");
    Ok(())
}

fn run_doctor(registry: &Registry) -> Result<()> {
    println!("anycli {}", env!("CARGO_PKG_VERSION"));
    println!("adapters:   {} loaded", registry.list().len());

    match anycli::hub::default_adapters_dir() {
        Some(dir) => {
            let count = if dir.is_dir() {
                std::fs::read_dir(&dir)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                matches!(
                                    e.path().extension().and_then(|x| x.to_str()),
                                    Some("yaml") | Some("yml")
                                )
                            })
                            .count()
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            println!(
                "user dir:   {} ({count} yaml)",
                dir.display()
            );
        }
        None => println!("user dir:   (home directory not found)"),
    }

    let browser = if anycli::browser::AgentBrowserFetcher::is_available() {
        "available (rsclaw or agent-browser)"
    } else {
        "not found (JSON/HTML adapters still work)"
    };
    println!("browser:    {browser}");
    println!("verbose:    anycli -v <adapter> <command>");
    Ok(())
}

fn print_adapter_help(adapter: &anycli::Adapter, sub_command: Option<&str>) {
    if let Some(cmd_name) = sub_command {
        if let Err(e) = print_command_help(adapter, cmd_name) {
            eprintln!("error: {e:#}");
        }
        return;
    }

    println!(
        "Usage: anycli {} [options] <command> [params...]\n",
        adapter.name
    );
    println!("{}\n", adapter.description);
    if !adapter.aliases.is_empty() {
        println!("Aliases: {}\n", adapter.aliases.join(", "));
    }
    println!("Commands:");

    let mut cmds: Vec<_> = adapter.commands.iter().collect();
    cmds.sort_by_key(|(name, _)| (*name).clone());

    for (cmd_name, cmd) in &cmds {
        let params_hint: String = cmd
            .params
            .iter()
            .filter(|(_, p)| p.required)
            .map(|(name, _)| format!("<{name}>"))
            .collect::<Vec<_>>()
            .join(" ");

        let opts = if cmd.params.iter().any(|(_, p)| !p.required) {
            "[options] "
        } else {
            ""
        };
        let label = if cmd.aliases.is_empty() {
            (*cmd_name).clone()
        } else {
            format!("{cmd_name}|{}", cmd.aliases.join("|"))
        };

        println!(
            "  {:<28} {}",
            format!("{} {}{}", label, opts, params_hint).trim(),
            cmd.description
        );
    }

    println!("\nOptions:");
    println!(
        "  -f, --format <fmt>         Output format: json, jsonc, table, csv, markdown, yaml, plain [default: table]"
    );
    println!("      --fields <cols>        Comma-separated columns to include");
    println!("      --sort <field>         Sort rows by field");
    println!("      --reverse              Reverse sort order");
    println!("      --compact              Compact JSON output");
    println!("      --timeout <secs>       Request timeout in seconds");
    println!("      --no-color             Disable ANSI colors");
    println!("  -h, --help                 Display help for command");
    println!(
        "\nRun 'anycli {} help <command>' for more info on a specific command.",
        adapter.name
    );
}

fn print_command_help(adapter: &anycli::Adapter, cmd_name: &str) -> Result<()> {
    let (canonical, cmd) = adapter.command(cmd_name).ok_or_else(|| {
        let available: Vec<&str> = adapter.commands.keys().map(|s| s.as_str()).collect();
        let hint = anycli::pipeline::suggest(cmd_name, available.iter().copied());
        anyhow::anyhow!(
            "command `{}` not found in adapter `{}`. available: {}{}",
            cmd_name,
            adapter.name,
            available.join(", "),
            hint
        )
    })?;

    println!("Usage: anycli {} {} [params...]\n", adapter.name, canonical);
    println!("{}\n", cmd.description);
    if !cmd.aliases.is_empty() {
        println!("Aliases: {}\n", cmd.aliases.join(", "));
    }

    if cmd.params.is_empty() {
        println!("No parameters.");
    } else {
        println!("Parameters:");
        let mut params: Vec<_> = cmd.params.iter().collect();
        params.sort_by_key(|(_, p)| !p.required);

        for (param_name, param) in &params {
            let req = if param.required { " (required)" } else { "" };
            let desc = param.description.as_deref().unwrap_or("");
            let default = param
                .default
                .as_ref()
                .map(|d| format!(" [default: {d}]"))
                .unwrap_or_default();
            let type_hint = &param.param_type;
            println!("  {:<14} <{type_hint}>  {desc}{default}{req}", param_name);
        }
    }

    println!("\nExample:");
    let example_params: String = cmd
        .params
        .iter()
        .filter(|(_, p)| p.required)
        .map(|(name, _)| format!("{name}=VALUE"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("  anycli {} {} {}", adapter.name, cmd_name, example_params);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_format_from_trailing_args() {
        let params = vec![
            "query=rust".into(),
            "--format".into(),
            "json".into(),
            "--fields".into(),
            "title,url".into(),
        ];
        let (flags, rest) = strip_output_flags(&params);
        assert_eq!(flags.format.as_deref(), Some("json"));
        assert_eq!(flags.fields.as_deref(), Some("title,url"));
        assert_eq!(rest, vec!["query=rust"]);
    }

    #[test]
    fn parse_key_value_and_flags() {
        let params = vec![
            "query=rust cli".into(),
            "--limit".into(),
            "5".into(),
            "--verbose".into(),
        ];
        let parsed = parse_params(&params, None);
        assert!(parsed.iter().any(|(k, v)| k == "query" && v == "rust cli"));
        assert!(parsed.iter().any(|(k, v)| k == "limit" && v == "5"));
        assert!(parsed.iter().any(|(k, v)| k == "verbose" && v == "true"));
    }
}
