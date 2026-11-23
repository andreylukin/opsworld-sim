use clap::{Parser, Subcommand};
use opsworld_sim::agent::llm::{self, ClaudeCli};
use opsworld_sim::agent::Policy;
use opsworld_sim::explore::{explore, StopOn};
use opsworld_sim::invariants::Invariant;
use opsworld_sim::render;
use opsworld_sim::repro::Repro;
use opsworld_sim::scenario::ScenarioKind;
use opsworld_sim::sim::run_in;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "opsworld",
    about = "Deterministic simulation testing for a logistics ops agent"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run many seeded scenarios and save a repro for each failure
    Explore {
        #[arg(long, default_value_t = 500)]
        runs: u64,
        #[arg(long, value_enum, default_value_t = Policy::V1)]
        policy: Policy,
        #[arg(long, value_enum, default_value_t = ScenarioKind::Baseline)]
        scenario: ScenarioKind,
        #[arg(long, default_value_t = 1)]
        start_seed: u64,
        /// Stop at the first failure of this invariant, or "any"
        #[arg(long, value_name = "INVARIANT")]
        stop_on: Option<String>,
        #[arg(long, default_value = "repros")]
        out: PathBuf,
    },
    /// Run seeds through a live model via the Claude Code CLI and record them
    Llm {
        /// Comma-separated seeds
        #[arg(long, value_delimiter = ',', required = true)]
        seeds: Vec<u64>,
        #[arg(long, default_value = "haiku")]
        model: String,
        #[arg(long, value_enum, default_value_t = ScenarioKind::Baseline)]
        scenario: ScenarioKind,
        /// Model turns allowed per document
        #[arg(long, default_value_t = llm::DEFAULT_MAX_TURNS)]
        max_turns: u32,
        #[arg(long, default_value = "repros")]
        out: PathBuf,
    },
    /// Re-run a saved repro and show its timeline
    Replay {
        path: PathBuf,
        /// Run the same seed and faults under a different policy
        #[arg(long, value_enum)]
        policy: Option<Policy>,
        /// Show every shipment, not only the one that failed
        #[arg(long)]
        full: bool,
    },
}

fn run_llm(
    seeds: &[u64],
    model: &str,
    scenario: ScenarioKind,
    max_turns: u32,
    out: &Path,
) -> Result<ExitCode, String> {
    let mut failed = 0;
    for &seed in seeds {
        let backend = ClaudeCli {
            model: model.to_string(),
        };
        let (result, transcript, _) =
            llm::run_model(scenario, seed, None, backend, model, max_turns)?;
        let repro = Repro::from_llm(&result, transcript);
        let path = repro.save(out)?;
        failed += u32::from(!result.passed());
        render::llm_run(&result, &repro, &path);
    }
    render::llm_summary(model, scenario, seeds.len(), failed);
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_explore(
    scenario: ScenarioKind,
    runs: u64,
    policy: Policy,
    start: u64,
    stop_on: Option<String>,
    out: &Path,
) -> Result<ExitCode, String> {
    if runs == 0 {
        return Err("--runs must be at least 1".to_string());
    }
    let stop = match stop_on {
        None => StopOn::Never,
        Some(text) => StopOn::parse(&text).ok_or_else(|| {
            let ids: Vec<&str> = Invariant::ALL.iter().map(|i| i.id()).collect();
            format!(
                "unknown invariant {text}, expected any or one of: {}",
                ids.join(", ")
            )
        })?,
    };
    render::explore_header(scenario, policy, start, runs);
    let summary = explore(scenario, start, runs, policy, stop, out, render::progress)?;
    render::explore_summary(&summary);
    Ok(if summary.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_replay(path: &Path, policy: Option<Policy>, full: bool) -> Result<ExitCode, String> {
    let repro = Repro::load(path)?;
    let policy = policy.unwrap_or(repro.policy);
    let result = match (&repro.transcript, policy) {
        (Some(transcript), Policy::Llm) => {
            match llm::replay(repro.scenario, repro.seed, &repro.faults, transcript) {
                Ok(result) => result,
                Err(message) => {
                    eprintln!("error: {message}");
                    return Ok(ExitCode::from(2));
                }
            }
        }
        (None, Policy::Llm) => return Err("LLM repro has no transcript".to_string()),
        _ => run_in(repro.scenario, repro.seed, &repro.faults, policy),
    };
    render::replay_header(&repro, policy);
    render::timeline(&result, &repro, full);
    render::replay_footer(&result, &repro, policy);
    Ok(
        if policy == repro.policy && result.hash != repro.trace_hash {
            ExitCode::from(2)
        } else if result.passed() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        },
    )
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::Explore {
            runs,
            policy,
            scenario,
            start_seed,
            stop_on,
            out,
        } => run_explore(scenario, runs, policy, start_seed, stop_on, &out),
        Command::Llm {
            seeds,
            model,
            scenario,
            max_turns,
            out,
        } => run_llm(&seeds, &model, scenario, max_turns, &out),
        Command::Replay { path, policy, full } => run_replay(&path, policy, full),
    };
    outcome.unwrap_or_else(|message| {
        eprintln!("error: {message}");
        ExitCode::from(3)
    })
}
