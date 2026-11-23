//! LLM mode, driven by scripted backends. Nothing here calls a live model.

use opsworld_sim::agent::llm::{self, Backend, Exchange, DEFAULT_MAX_TURNS};
use opsworld_sim::faults::Fault;
use opsworld_sim::invariants::Invariant;
use opsworld_sim::repro::Repro;
use opsworld_sim::scenario::ScenarioKind;
use opsworld_sim::tools::WriteKind;
use serde_json::Value;

const SEED: u64 = 7;

fn lost_ack() -> Vec<Fault> {
    vec![Fault::TimeoutAfterCommit {
        tool: WriteKind::PostReceipt,
        nth: 0,
    }]
}

fn document(prompt: &str) -> Value {
    let line = prompt.lines().nth(3).expect("prompt has a document line");
    serde_json::from_str(line).expect("document line is JSON")
}

/// Posts receipts and handles a timeout either blindly or by reading first.
struct Scripted {
    reads_after_timeout: bool,
}

impl Backend for Scripted {
    fn complete(&mut self, _system: &str, prompt: &str) -> Result<String, String> {
        let doc = document(prompt);
        let s = &doc["shipment"];
        if doc["type"] != "STATUS arrived" {
            return Ok(r#"{"done":true}"#.to_string());
        }
        let qty = doc["fields"]["qty_received"].as_str().unwrap();
        let post = format!(r#"{{"tool":"post_receipt","args":{{"shipment":{s},"qty":{qty}}}}}"#);
        let last = prompt.lines().rev().nth(2).unwrap_or("");
        if !prompt.contains("tool calls so far") {
            Ok(post)
        } else if last.contains("timeout") {
            Ok(if self.reads_after_timeout {
                format!(r#"{{"tool":"read_shipment","args":{{"shipment":{s}}}}}"#)
            } else {
                post
            })
        } else {
            Ok(r#"{"done":true}"#.to_string())
        }
    }
}

fn record(reads_after_timeout: bool) -> Repro {
    let faults = lost_ack();
    let backend = Scripted {
        reads_after_timeout,
    };
    let (result, transcript, _) = llm::run_model(
        ScenarioKind::Baseline,
        SEED,
        Some(&faults),
        backend,
        "scripted",
        DEFAULT_MAX_TURNS,
    )
    .unwrap();
    Repro::from_llm(&result, transcript)
}

fn replay(repro: &Repro) -> Result<String, String> {
    let transcript = repro.transcript.as_ref().unwrap();
    llm::replay(repro.scenario, repro.seed, &repro.faults, transcript).map(|r| r.hash)
}

#[test]
fn blind_retry_by_a_model_is_caught() {
    let repro = record(false);
    assert_eq!(repro.invariant, Some(Invariant::NoBlindRetry));
}

#[test]
fn model_that_reads_after_timeout_passes() {
    assert_eq!(record(true).invariant, None);
}

#[test]
fn saved_transcript_replays_to_the_same_hash() {
    let dir = std::env::temp_dir().join(format!("opsworld-llm-test-{}", std::process::id()));
    let repro = record(false);
    let loaded = Repro::load(&repro.save(&dir).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(loaded, repro);
    assert_eq!(replay(&loaded).unwrap(), repro.trace_hash);
}

#[test]
fn replay_rejects_a_changed_request() {
    let mut repro = record(false);
    repro.transcript.as_mut().unwrap().exchanges[0].request += " ";
    assert!(replay(&repro).unwrap_err().contains("transcript mismatch"));
}

#[test]
fn replay_rejects_missing_and_extra_exchanges() {
    let mut short = record(false);
    short.transcript.as_mut().unwrap().exchanges.pop();
    assert!(replay(&short).unwrap_err().contains("transcript mismatch"));

    let mut long = record(false);
    long.transcript.as_mut().unwrap().exchanges.push(Exchange {
        request: "extra".to_string(),
        response: "{}".to_string(),
    });
    assert!(replay(&long).unwrap_err().contains("never requested"));
}

#[test]
fn replay_rejects_a_changed_system_prompt() {
    let mut repro = record(false);
    repro.transcript.as_mut().unwrap().system.push('!');
    assert!(replay(&repro).unwrap_err().contains("system prompt"));
}

struct Babbles;

impl Backend for Babbles {
    fn complete(&mut self, _system: &str, _prompt: &str) -> Result<String, String> {
        Ok("I am not sure what to do.".to_string())
    }
}

#[test]
fn unparseable_replies_stop_at_the_turn_cap() {
    let (_, transcript, _) =
        llm::run_model(ScenarioKind::Baseline, SEED, Some(&[]), Babbles, "x", 2).unwrap();
    assert!(transcript.turn_cap_hits > 0);
    assert_eq!(
        transcript.exchanges.len() as u32,
        transcript.turn_cap_hits * 2
    );
    assert!(transcript.exchanges[1].request.contains("bad_reply"));
}

struct Down;

impl Backend for Down {
    fn complete(&mut self, _system: &str, _prompt: &str) -> Result<String, String> {
        Err("backend unavailable".to_string())
    }
}

#[test]
fn backend_failure_is_an_error_and_not_a_violation() {
    let outcome = llm::run_model(ScenarioKind::Baseline, SEED, Some(&[]), Down, "x", 2);
    assert!(outcome.err().unwrap().contains("backend unavailable"));
}

/// A real recorded model run, replayed offline.
#[test]
fn committed_live_recording_replays_exactly() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/llm-seed-00010.json");
    let repro = Repro::load(&path).unwrap();
    assert_eq!(replay(&repro).unwrap(), repro.trace_hash);
}
