//! AnyCLI — turn any website into structured CLI output.

use std::process;

use anyhow::Result;
use clap::{Parser, Subcommand};

use anycli::{OutputFormat, Pipeline, Registry};

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
        /// Command name (e.g., "top", "search").
        command: String,
        /// Parameters as key=value pairs (e.g., limit=10 query="rust").
        #[arg(trailing_var_arg = true)]
        params: Vec<String>,
        /// Output format: json, table, csv, markdown.
        #[arg(long, short, default_value = "json")]
        format: String,
    },
    /// List all available adapters.
    List,
    /// Show details of a specific adapter.
    Info {
        /// Adapter name.
        adapter: String,
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
    }

    Ok(())
}
