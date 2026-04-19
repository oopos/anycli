//! OpenCLI — turn any website into structured CLI output.
//!
//! Declarative YAML adapters define how to extract data from websites.
//! The pipeline engine fetches, parses, extracts, and formats the data.
//!
//! # Usage
//!
//! ```no_run
//! use anycli::{Registry, Pipeline, OutputFormat};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let registry = Registry::load()?;
//!     let adapter = registry.find("hackernews")?;
//!     let result = Pipeline::execute(&adapter, "top", &[("limit", "10")]).await?;
//!     println!("{}", result.format(OutputFormat::Json)?);
//!     Ok(())
//! }
//! ```

pub mod adapter;
pub mod hub;
pub mod output;
pub mod pipeline;
pub mod registry;

pub use adapter::Adapter;
pub use hub::Hub;
pub use output::OutputFormat;
pub use pipeline::{Pipeline, PipelineResult};
pub use registry::Registry;
