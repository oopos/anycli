//! AnyCLI — turn any website into structured CLI output.

use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};

use anycli::{Hub, OutputFormat, Pipeline, Registry};

#[derive(Parser)]
#[command(name = "anycli", version, about = "Turn any website into structured CLI output")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an adapter command to extract data from a website.
    Run {
        /// Adapter name (e.g., "hackernews", "bilibili").
        adapter: String,
        /// Command name (e.g., "top", "search", "help", "--help").
        command: String,
        /// Parameters as key=value pairs (e.g., limit=10 query="rust").
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        params: Vec<String>,
        /// Output format: table, json, md, yaml, csv.
        #[arg(long, short, default_value = "table")]
        format: String,
    },
    /// List all available adapters.
    List,
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
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

async fn run() -> Result<()> {
    // Intercept `anycli run <adapter> --help` and `--format` before clap parses.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "run" {
        // Handle --help
        let has_help = args[2..].iter().any(|a| a == "--help" || a == "-h");
        if has_help {
            let registry = Registry::load()?;
            let adapter_name = &args[2];
            if let Ok(adapter) = registry.find(adapter_name) {
                let cmd_name = args[3..].iter().find(|a| *a != "--help" && *a != "-h");
                if let Some(cmd) = cmd_name {
                    print_command_help(adapter, cmd)?;
                } else {
                    print_adapter_help(adapter, None);
                }
                return Ok(());
            }
        }

        // Extract --format / -f from args before clap (since trailing_var_arg eats it)
        let mut format_override: Option<String> = None;
        let mut filtered_args: Vec<String> = Vec::new();
        let mut skip_next = false;
        for (i, arg) in args.iter().enumerate() {
            if skip_next { skip_next = false; continue; }
            if arg == "--format" || arg == "-f" {
                if let Some(val) = args.get(i + 1) {
                    format_override = Some(val.clone());
                    skip_next = true;
                    continue;
                }
            }
            if let Some(val) = arg.strip_prefix("--format=") {
                format_override = Some(val.to_string());
                continue;
            }
            filtered_args.push(arg.clone());
        }

        if format_override.is_some() {
            // Re-run with filtered args + format injected as clap arg
            let fmt = format_override.unwrap();
            let registry = Registry::load()?;

            // Parse adapter, command, params from filtered_args
            // filtered_args: [binary, "run", adapter, command, ...params]
            if filtered_args.len() >= 4 {
                let adapter_name = &filtered_args[2];
                let command = &filtered_args[3];
                let params = &filtered_args[4..];

                // Handle help
                if command == "help" || command == "--help" || command == "-h" {
                    let adapter = registry.find(adapter_name)?;
                    print_adapter_help(adapter, params.first().map(|s| s.as_str()));
                    return Ok(());
                }

                let adapter = registry.find(adapter_name)?;
                let fmt: OutputFormat = fmt.parse()?;

                let parsed: Vec<(String, String)> = params
                    .iter()
                    .filter(|p| !p.starts_with('-'))
                    .filter_map(|p| {
                        let (k, v) = p.split_once('=')?;
                        Some((k.to_owned(), v.to_owned()))
                    })
                    .collect();

                let param_refs: Vec<(&str, &str)> = parsed
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();

                let result = Pipeline::execute(adapter, command, &param_refs).await?;
                println!("{}", result.format(fmt)?);
                return Ok(());
            }
        }
    }

    let cli = Cli::parse();
    let registry = Registry::load()?;

    match cli.command {
        Commands::List => {
            println!("{:<20} {}", "ADAPTER", "DESCRIPTION");
            println!("{:<20} {}", "-------", "-----------");
            for adapter in registry.list() {
                println!("{:<20} {}", adapter.name, adapter.description);
            }
        }

        Commands::Info { adapter: name } => {
            let adapter = registry.find(&name)?;
            println!("Name:        {}", adapter.name);
            println!("Description: {}", adapter.description);
            println!("Base URL:    {}", adapter.base_url);
            if !adapter.version.is_empty() {
                println!("Version:     {}", adapter.version);
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
        } => {
            let adapter = registry.find(&name)?;

            // Show adapter help: `anycli run <adapter> help` or `anycli run <adapter> --help`
            if command == "help" || command == "--help" || command == "-h" {
                print_adapter_help(adapter, params.first().map(|s| s.as_str()));
                return Ok(());
            }

            // Show command help: `anycli run <adapter> <cmd> --help`
            if params.iter().any(|p| p == "--help" || p == "-h" || p == "help") {
                print_command_help(adapter, &command)?;
                return Ok(());
            }

            let fmt: OutputFormat = format.parse()?;

            // Parse key=value params.
            let parsed: Vec<(String, String)> = params
                .iter()
                .filter_map(|p| {
                    let (k, v) = p.split_once('=')?;
                    Some((k.to_owned(), v.to_owned()))
                })
                .collect();

            let param_refs: Vec<(&str, &str)> = parsed
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            let result = Pipeline::execute(adapter, &command, &param_refs).await?;
            println!("{}", result.format(fmt)?);
        }

        Commands::Search { query } => {
            let hub = Hub::new()?;
            let results = hub.search(&query).await?;
            if results.is_empty() {
                println!("No adapters found for `{query}`");
            } else {
                println!("{:<20} {}", "ADAPTER", "DESCRIPTION");
                println!("{:<20} {}", "-------", "-----------");
                for entry in &results {
                    println!("{:<20} {}", entry.name, entry.description);
                }
                println!("\nInstall: anycli install <name>");
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
    }

    Ok(())
}

fn print_adapter_help(adapter: &anycli::Adapter, sub_command: Option<&str>) {
    // If a specific command is requested: `anycli run <adapter> help <command>`
    if let Some(cmd_name) = sub_command {
        if let Err(e) = print_command_help(adapter, cmd_name) {
            eprintln!("error: {e:#}");
        }
        return;
    }

    println!("Usage: anycli run {} [options] <command> [params...]\n", adapter.name);
    println!("{}\n", adapter.description);
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

        println!(
            "  {:<28} {}",
            format!("{} {}{}", cmd_name, opts, params_hint).trim(),
            cmd.description
        );
    }

    println!("\nOptions:");
    println!("  -f, --format <fmt>         Output format: json, table, csv, markdown [default: json]");
    println!("  -h, --help                 Display help for command");
    println!(
        "\nRun 'anycli run {} help <command>' for more info on a specific command.",
        adapter.name
    );
}

fn print_command_help(adapter: &anycli::Adapter, cmd_name: &str) -> Result<()> {
    let cmd = adapter
        .commands
        .get(cmd_name)
        .ok_or_else(|| {
            let available: Vec<&str> = adapter.commands.keys().map(|s| s.as_str()).collect();
            anyhow::anyhow!(
                "command `{}` not found in adapter `{}`. available: {}",
                cmd_name,
                adapter.name,
                available.join(", ")
            )
        })?;

    println!(
        "Usage: anycli run {} {} [params...]\n",
        adapter.name, cmd_name
    );
    println!("{}\n", cmd.description);

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
    println!("  anycli run {} {} {}", adapter.name, cmd_name, example_params);

    Ok(())
}
