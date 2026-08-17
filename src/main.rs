use std::path::PathBuf;
use std::process::exit;

use clap::{Parser, Subcommand};

use tauri_tutorial_runner::error::{Error, Result};
use tauri_tutorial_runner::runner::{run_tutorial, RunOptions};
use tauri_tutorial_runner::tutorial::{load_tutorial, Tutorial};
use tauri_tutorial_runner::{manifest, revendor, schema, snapshot};

#[derive(Parser)]
#[command(
  name = "tatu",
  about = "Executable, verifiable Tauri tutorials",
  long_about = "tatu runs tutorials declared as base scaffold + mutations + assertions.\n\
    `check` is an advisory run on the host toolchain; `run` is the authoritative,\n\
    container-only mode that produces the committed tutorial.manifest.json.",
  version
)]
struct Cli {
  #[command(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  /// Advisory run on the host; never authoritative
  Check {
    /// tutorial directory (contains tutorial.yaml)
    dir: PathBuf,
    /// run a single step only
    #[arg(long)]
    step: Option<String>,
  },
  /// Authoritative run (container); writes the manifest
  Run {
    dir: PathBuf,
    #[arg(long)]
    step: Option<String>,
    /// set inside the pinned container image
    // BoolishValueParser so `TATU_CONTAINER=1` parses; plain bool rejects anything but true/false
    #[arg(long, env = "TATU_CONTAINER", hide = true, default_value_t = false, value_parser = clap::builder::BoolishValueParser::new())]
    container: bool,
    /// explicit override to run authoritatively outside the container
    #[arg(long, env = "TATU_ALLOW_LOCAL", hide = true, default_value_t = false, value_parser = clap::builder::BoolishValueParser::new())]
    allow_local: bool,
  },
  /// Parse and validate tutorial.yaml only
  Validate { dir: PathBuf },
  /// Re-scaffold a pool base with create-tauri-app (dir name: <template>@<cta-version>)
  Revendor { dir: PathBuf },
  /// Compare the last run's manifest against the committed expected one
  Verify { dir: PathBuf },
  /// Accept the last run's manifest as the committed expected one
  Bless { dir: PathBuf },
  /// Emit JSON Schemas (and optionally TypeScript) for the contracts
  Schema {
    /// output directory for the JSON Schemas
    #[arg(long, default_value = "schemas")]
    out: PathBuf,
    /// also write the manifest contract as TypeScript interfaces to this path
    #[arg(long)]
    emit_ts: Option<PathBuf>,
  },
}

fn main() {
  env_logger::init();
  if let Err(e) = run() {
    eprintln!("tatu: {e}");
    exit(1);
  }
}

fn run() -> Result<()> {
  match Cli::parse().command {
    Commands::Check { dir, step } => {
      let tutorial = load_and_announce(&dir)?;
      run_tutorial(
        &tutorial,
        &RunOptions {
          authoritative: false,
          only_step: step,
        },
      )
    }
    Commands::Run {
      dir,
      step,
      container,
      allow_local,
    } => {
      if !container && !allow_local {
        return Err(Error::Runner(
          "run is container-only (authoritative). Use `tatu check` on the host, or set TATU_ALLOW_LOCAL=1 to override.".into(),
        ));
      }
      let tutorial = load_and_announce(&dir)?;
      run_tutorial(
        &tutorial,
        &RunOptions {
          authoritative: true,
          only_step: step,
        },
      )
    }
    Commands::Validate { dir } => {
      load_and_announce(&dir)?;
      Ok(())
    }
    Commands::Revendor { dir } => revendor::revendor(&dir),
    Commands::Verify { dir } => manifest::verify_expected(&load_and_announce(&dir)?),
    Commands::Bless { dir } => manifest::bless_expected(&load_and_announce(&dir)?),
    Commands::Schema { out, emit_ts } => {
      std::fs::create_dir_all(&out)?;
      let tutorial_schema = schemars::schema_for!(Tutorial);
      let tutorial_path = out.join("tutorial.schema.json");
      let manifest_path = out.join(snapshot::SCHEMA_FILE);
      std::fs::write(
        &tutorial_path,
        serde_json::to_string_pretty(&tutorial_schema)? + "\n",
      )?;
      // the same rendering `tatu bless` writes next to a tutorial tree
      std::fs::write(&manifest_path, manifest::schema_json()?)?;
      println!("wrote {}", tutorial_path.display());
      println!("wrote {}", manifest_path.display());
      if let Some(ts_path) = emit_ts {
        std::fs::write(&ts_path, schema::manifest_types_ts()?)?;
        println!("wrote {}", ts_path.display());
      }
      Ok(())
    }
  }
}

fn load_and_announce(dir: &PathBuf) -> Result<Tutorial> {
  let dir = std::path::absolute(dir)?;
  let tutorial = load_tutorial(&dir)?;
  println!(
    "loaded {}: \"{}\" ({} steps)",
    tutorial.id,
    tutorial.title,
    tutorial.steps.len()
  );
  Ok(tutorial)
}
