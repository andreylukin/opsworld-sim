//! Terminal output. Color is applied here only, after hashing.

use crate::agent::Policy;
use crate::explore::Summary;
use crate::invariants::Invariant;
use crate::model::clock;
use crate::repro::Repro;
use crate::scenario::ScenarioKind;
use crate::sim::{EventKind, RunResult, TraceEvent};
use colored::Colorize;
use std::io::Write;
use std::path::Path;

const DOTS_PER_LINE: u64 = 50;

/// The before/after shrink block, uncolored so tests can check it.
pub fn shrink_lines(repro: &Repro) -> Vec<String> {
    let (before, after) = (repro.faults_before.len(), repro.faults.len());
    let plural = if before == 1 { "" } else { "s" };
    let mut lines = vec![format!(
        "shrink: {before} fault{plural} -> {after} (seed {}, every remaining fault is necessary)",
        repro.seed
    )];
    lines.push("  before:".to_string());
    lines.extend(repro.faults_before.iter().map(|f| format!("    {f}")));
    lines.push("  after:".to_string());
    lines.extend(repro.faults.iter().map(|f| format!("    {f}")));
    lines
}

fn print_shrink(repro: &Repro) {
    for line in shrink_lines(repro) {
        if line.starts_with("    ") {
            println!("{}", line.yellow());
        } else {
            println!("{}", line.bold());
        }
    }
}

pub fn explore_header(scenario: ScenarioKind, policy: Policy, start: u64, runs: u64) {
    println!(
        "{}  scenario={}  policy={}  seeds={}..{}",
        "opsworld explore".bold(),
        scenario,
        policy.to_string().cyan().bold(),
        start,
        start + runs - 1
    );
    println!();
}

pub fn progress(done: u64, passed: bool) {
    if passed {
        print!("{}", ".".green());
    } else {
        print!("{}", "F".red().bold());
    }
    if done.is_multiple_of(DOTS_PER_LINE) {
        println!(" {done}");
    }
    let _ = std::io::stdout().flush();
}

pub fn explore_summary(summary: &Summary) {
    if !summary.runs.is_multiple_of(DOTS_PER_LINE) {
        println!();
    }
    println!();
    let passed = summary.runs - summary.failed;
    println!(
        "runs {}   passed {}   failed {}   faults injected {}",
        summary.runs.to_string().bold(),
        passed.to_string().green().bold(),
        if summary.failed == 0 {
            "0".green().bold()
        } else {
            summary.failed.to_string().red().bold()
        },
        summary.faults_injected,
    );

    if !summary.by_invariant.is_empty() {
        println!();
        println!("{}", "invariants fired".bold());
        for inv in Invariant::ALL {
            if let Some(count) = summary.by_invariant.get(&inv) {
                println!("  {:<30} {:>4}", inv.id().red(), count);
            }
        }
    }

    if let Some((repro, path)) = &summary.stopped {
        println!();
        println!(
            "{} seed {} violated {}",
            " STOPPED ".on_red().white().bold(),
            repro.seed.to_string().bold(),
            repro.invariant.map_or("none", Invariant::id).red().bold()
        );
        print_shrink(repro);
        println!("  repro: {}", path.display());
        println!("  replay: cargo run --release -- replay {}", path.display());
    } else if let Some(path) = &summary.first_repro {
        println!();
        println!(
            "shrunk {} failing schedules: {} faults -> {}",
            summary.failed, summary.faults_before_shrink, summary.faults_after_shrink
        );
        if let Some(repro) = &summary.best_shrink {
            print_shrink(repro);
        }
        println!();
        println!("repros saved, first: {}", path.display());
    } else {
        println!();
        println!("{}", "all invariants held on every seed".green().bold());
    }
}

fn paint(event: &TraceEvent) -> String {
    let tag = format!("{:<9}", event.kind.tag());
    let head = format!("{:04} {}", event.step, clock(event.at));
    match event.kind {
        EventKind::Violation => format!(
            "{} {} {}",
            head.red().bold(),
            tag.on_red().white().bold(),
            event.text.red().bold()
        ),
        EventKind::Fault => format!(
            "{} {} {}",
            head.dimmed(),
            tag.yellow().bold(),
            event.text.yellow()
        ),
        EventKind::Write => format!("{} {} {}", head.dimmed(), tag.cyan().bold(), event.text),
        EventKind::Read => format!("{} {} {}", head.dimmed(), tag.dimmed(), event.text.dimmed()),
        EventKind::Deliver => format!("{} {} {}", head.dimmed(), tag.blue(), event.text),
        EventKind::Rejected => format!("{} {} {}", head.dimmed(), tag.magenta(), event.text),
        EventKind::Escalate => format!(
            "{} {} {}",
            head.dimmed(),
            tag.green().bold(),
            event.text.green()
        ),
        EventKind::End => format!(
            "{} {} {}",
            head.dimmed(),
            tag.green().bold(),
            event.text.green()
        ),
    }
}

/// Prints the timeline, focused on one shipment unless `full` is set.
pub fn timeline(result: &RunResult, repro: &Repro, full: bool) {
    let focus = repro.shipment.filter(|_| !full);
    let mut hidden = 0;
    for event in &result.trace {
        let shown = match focus {
            Some(s) => event.shipment == Some(s) || event.shipment.is_none(),
            None => true,
        };
        if shown {
            println!("{}", paint(event));
        } else {
            hidden += 1;
        }
    }
    if let (Some(s), true) = (focus, hidden > 0) {
        println!(
            "{}",
            format!("({hidden} events for shipments other than {s} hidden, --full shows them)")
                .dimmed()
        );
    }
}

pub fn replay_header(repro: &Repro, policy: Policy) {
    println!(
        "{}  seed={}  policy={}",
        "opsworld replay".bold(),
        repro.seed.to_string().bold(),
        policy.to_string().cyan().bold()
    );
    println!(
        "scenario: {}  recorded violation: {} ({})",
        repro.scenario,
        repro.invariant.map_or("none", Invariant::id).red(),
        repro.policy
    );
    if let Some(transcript) = &repro.transcript {
        println!(
            "model: {}  recorded exchanges: {}  (replayed from the file, live model not called)",
            transcript.model,
            transcript.exchanges.len()
        );
    }
    println!("faults:");
    for fault in &repro.faults {
        println!("  {}", fault.to_string().yellow());
    }
    println!();
}

pub fn replay_footer(result: &RunResult, repro: &Repro, policy: Policy) {
    println!();
    let short = &result.hash[..16];
    if policy == repro.policy {
        if result.hash == repro.trace_hash {
            println!(
                "{} trace hash {} matches the recording",
                " EXACT REPLAY ".on_green().black().bold(),
                short
            );
        } else {
            println!(
                "{} trace hash {} differs from recorded {}",
                " REPLAY MISMATCH ".on_red().white().bold(),
                short,
                repro.trace_hash.get(..16).unwrap_or(&repro.trace_hash)
            );
        }
    } else if result.passed() {
        println!(
            "{} policy {} held every invariant under the same seed and faults (trace {})",
            " PASS ".on_green().black().bold(),
            policy,
            short
        );
    } else {
        println!(
            "{} policy {} violated {} (trace {})",
            " FAIL ".on_red().white().bold(),
            policy,
            result.violations[0].invariant,
            short
        );
    }
}

pub fn llm_run(result: &RunResult, repro: &Repro, path: &Path) {
    let exchanges = repro.transcript.as_ref().map_or(0, |t| t.exchanges.len());
    let capped = repro.transcript.as_ref().map_or(0, |t| t.turn_cap_hits);
    let verdict = if result.passed() {
        " PASS ".on_green().black().bold()
    } else {
        " FAIL ".on_red().white().bold()
    };
    println!(
        "{verdict} seed {}  faults {}  model exchanges {exchanges}  turn-cap hits {capped}  trace {}",
        result.seed,
        result.faults.len(),
        &result.hash[..16],
    );
    for fault in &result.faults {
        println!("  fault: {}", fault.to_string().yellow());
    }
    for v in &result.violations {
        println!("  {}: {}", v.invariant.id().red().bold(), v.detail);
    }
    println!("  repro: {}", path.display());
}

pub fn llm_summary(model: &str, scenario: ScenarioKind, seeds: usize, failed: u32) {
    println!();
    println!(
        "llm  model={model}  scenario={scenario}  seeds {seeds}  passed {}  failed {failed}",
        seeds as u32 - failed
    );
}
