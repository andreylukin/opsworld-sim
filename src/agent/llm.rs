//! An agent driven by a real model through the same tool port as the scripted
//! policies. Live runs shell out to the Claude Code CLI. Every exchange is
//! recorded, and a replay answers from the recording without calling a model.

use super::Agent;
use crate::faults::{generate_schedule, Fault};
use crate::model::{Document, Minutes, ShipmentId};
use crate::scenario::{Scenario, ScenarioKind};
use crate::sim::{run, RunResult};
use crate::tools::{ToolCall, ToolError, ToolPort, ToolReply, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

pub const DEFAULT_MAX_TURNS: u32 = 12;
/// Bounds model-supplied numbers so they cannot overflow simulated arithmetic.
const MAX_ARG: u64 = 1_000_000;

pub const SYSTEM_PROMPT: &str = r#"You are an operations agent for an inbound warehouse. You receive one logistics document at a time and keep the ERP and TMS correct by calling tools. Times are integer minutes since the start of the scenario.

Business rules:
- ASN: compare qty_shipped with the PO's ordered quantity. If short, call flag_mismatch and do not confirm a promise date. Find and book a dock at or after the ETA. If shipped in full, set the promise date to the dock start plus 120.
- Status with a changed ETA: move the dock appointment and promise date to match.
- no_show: cancel the dock appointment and retract the promise date until a new ETA arrives.
- arrived: post a receipt for qty_received.
- If a document cannot be trusted or you cannot proceed safely, escalate and make no other writes for that shipment.

Reply with exactly one JSON object and no other text. Either call one tool:
{"tool":"<name>","args":{...}}
or finish this document:
{"done":true}

Tools:
read_po {shipment}
read_shipment {shipment}
find_slot {shipment, earliest}
book_dock {shipment, door, start}
cancel_dock {shipment}
set_promise {shipment, at} (at is minutes, or null to retract)
post_receipt {shipment, qty}
flag_mismatch {shipment}
escalate {shipment, reason}"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    pub request: String,
    pub response: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub model: String,
    pub system: String,
    pub max_turns: u32,
    /// Documents abandoned because the model hit the turn cap.
    pub turn_cap_hits: u32,
    pub exchanges: Vec<Exchange>,
}

pub trait Backend {
    fn complete(&mut self, system: &str, prompt: &str) -> Result<String, String>;
}

/// Calls `claude -p` with tools, settings and session storage turned off.
pub struct ClaudeCli {
    pub model: String,
}

impl Backend for ClaudeCli {
    fn complete(&mut self, system: &str, prompt: &str) -> Result<String, String> {
        let mut child = Command::new("claude")
            .args(["-p", "--output-format", "json", "--no-session-persistence"])
            .args([
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--tools",
                "",
            ])
            .args(["--model", &self.model, "--system-prompt", system])
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the claude CLI: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("claude stdin unavailable")?
            .write_all(prompt.as_bytes())
            .map_err(|e| format!("cannot write to claude: {e}"))?;
        let output = child
            .wait_with_output()
            .map_err(|e| format!("claude did not finish: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "claude exited with {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
                String::from_utf8_lossy(&output.stdout).trim(),
            ));
        }
        let reply: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("claude returned invalid JSON: {e}"))?;
        if reply["is_error"].as_bool().unwrap_or(false) {
            return Err(format!("claude reported an error: {}", reply["result"]));
        }
        reply["result"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "claude reply has no result text".to_string())
    }
}

/// Answers from a recording, and fails if the agent asks anything else.
pub struct Replay {
    exchanges: std::vec::IntoIter<Exchange>,
    index: usize,
}

impl Replay {
    pub fn new(exchanges: Vec<Exchange>) -> Self {
        Self {
            exchanges: exchanges.into_iter(),
            index: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.exchanges.len()
    }
}

impl Backend for Replay {
    fn complete(&mut self, _system: &str, prompt: &str) -> Result<String, String> {
        let at = self.index;
        self.index += 1;
        let exchange = self
            .exchanges
            .next()
            .ok_or_else(|| format!("transcript mismatch: no recorded exchange {at}"))?;
        if exchange.request != prompt {
            return Err(format!(
                "transcript mismatch: request {at} differs from the recording"
            ));
        }
        Ok(exchange.response)
    }
}

enum Action {
    Done,
    Call(ToolCall),
}

fn number(args: &Value, key: &str) -> Result<u32, String> {
    args[key]
        .as_u64()
        .filter(|n| *n <= MAX_ARG)
        .map(|n| n as u32)
        .ok_or_else(|| format!("{key} must be an integer from 0 to {MAX_ARG}"))
}

fn parse_action(text: &str) -> Result<Action, String> {
    let (start, end) = text
        .find('{')
        .zip(text.rfind('}'))
        .ok_or("reply is not a JSON object")?;
    let value: Value =
        serde_json::from_str(&text[start..=end]).map_err(|e| format!("invalid JSON: {e}"))?;
    if value["done"] == json!(true) {
        return Ok(Action::Done);
    }
    let tool = value["tool"].as_str().ok_or("missing tool name")?;
    let args = &value["args"];
    let s = ShipmentId(number(args, "shipment")?);
    let call = match tool {
        "read_po" => ToolCall::ReadPo(s),
        "read_shipment" => ToolCall::ReadShipment(s),
        "find_slot" => ToolCall::FindSlot {
            shipment: s,
            earliest: number(args, "earliest")?,
        },
        "book_dock" => ToolCall::BookDock {
            shipment: s,
            door: number(args, "door")?,
            start: number(args, "start")?,
        },
        "cancel_dock" => ToolCall::CancelDock(s),
        "set_promise" => ToolCall::SetPromise {
            shipment: s,
            at: match args.get("at") {
                Some(Value::Null) => None,
                Some(_) => Some(number(args, "at")?),
                None => return Err("set_promise needs at (minutes or null)".to_string()),
            },
        },
        "post_receipt" => ToolCall::PostReceipt {
            shipment: s,
            qty: number(args, "qty")?,
        },
        "flag_mismatch" => ToolCall::FlagMismatch(s),
        "escalate" => ToolCall::Escalate {
            shipment: s,
            reason: args["reason"].as_str().unwrap_or("unspecified").to_string(),
        },
        other => return Err(format!("unknown tool {other}")),
    };
    Ok(Action::Call(call))
}

fn reply_json(result: &ToolResult) -> String {
    let value = match result {
        Ok(ToolReply::Po { ordered }) => json!({ "ordered": ordered }),
        Ok(ToolReply::Shipment(v)) => json!({
            "ordered": v.ordered,
            "receipts": v.receipts,
            "dock_appointment": v.appointment,
            "promise_date": v.promise,
            "flagged": v.flagged,
        }),
        Ok(ToolReply::Slot { door, start }) => json!({ "door": door, "start": start }),
        Ok(ToolReply::Done) => json!({ "ok": true }),
        Err(ToolError::Timeout) => json!({ "error": "timeout" }),
        Err(ToolError::Rejected(reason)) => json!({ "error": "rejected", "reason": reason }),
        Err(ToolError::Halted) => json!({ "error": "halted" }),
    };
    value.to_string()
}

fn doc_json(doc: &Document) -> String {
    json!({
        "control": doc.control,
        "type": doc.kind.to_string(),
        "shipment": doc.shipment.0,
        "occurred_at": doc.occurred_at,
        "fields": doc.fields,
    })
    .to_string()
}

fn prompt(now: Minutes, doc: &str, earlier: &[String], steps: &[String]) -> String {
    let mut text = format!("now: {now}\n\ndocument:\n{doc}\n");
    if !earlier.is_empty() {
        text += "\nearlier documents for this shipment, in delivery order:\n";
        text += &earlier.join("\n");
        text += "\n";
    }
    if !steps.is_empty() {
        text += "\ntool calls so far for this document:\n";
        text += &steps.join("\n");
        text += "\n";
    }
    text + "\nReply with the next JSON object."
}

pub struct LlmAgent<B: Backend> {
    backend: B,
    max_turns: u32,
    history: BTreeMap<ShipmentId, Vec<String>>,
    exchanges: Vec<Exchange>,
    turn_cap_hits: u32,
    failure: Option<String>,
}

impl<B: Backend> LlmAgent<B> {
    pub fn new(backend: B, max_turns: u32) -> Self {
        Self {
            backend,
            max_turns,
            history: BTreeMap::new(),
            exchanges: Vec::new(),
            turn_cap_hits: 0,
            failure: None,
        }
    }
}

impl<B: Backend> Agent for LlmAgent<B> {
    fn name(&self) -> &'static str {
        "llm"
    }

    fn handle(&mut self, doc: &Document, port: &mut dyn ToolPort) {
        if self.failure.is_some() {
            return;
        }
        let rendered = doc_json(doc);
        let earlier = self.history.entry(doc.shipment).or_default().clone();
        let mut steps = Vec::new();
        let mut finished = false;
        for _ in 0..self.max_turns {
            let request = prompt(port.now(), &rendered, &earlier, &steps);
            let response = match self.backend.complete(SYSTEM_PROMPT, &request) {
                Ok(response) => response,
                Err(message) => {
                    self.failure = Some(message);
                    return;
                }
            };
            self.exchanges.push(Exchange {
                request,
                response: response.clone(),
            });
            let said = response.trim().replace('\n', " ");
            match parse_action(&response) {
                Ok(Action::Done) => {
                    finished = true;
                    break;
                }
                Ok(Action::Call(call)) => {
                    let result = port.call(call);
                    if result == Err(ToolError::Halted) {
                        finished = true;
                        break;
                    }
                    steps.push(format!("> {said}\n< {}", reply_json(&result)));
                }
                Err(problem) => {
                    let error = json!({ "error": "bad_reply", "reason": problem });
                    steps.push(format!("> {said}\n< {error}"));
                }
            }
        }
        self.turn_cap_hits += u32::from(!finished);
        self.history.entry(doc.shipment).or_default().push(rendered);
    }
}

/// Runs one seed through a model and returns the result with its recording.
/// A backend failure is an error, never a simulated violation.
pub fn run_model<B: Backend>(
    kind: ScenarioKind,
    seed: u64,
    faults: Option<&[Fault]>,
    backend: B,
    model: &str,
    max_turns: u32,
) -> Result<(RunResult, Transcript, B), String> {
    let scenario = Scenario::build(kind, seed);
    let schedule = faults.map_or_else(|| generate_schedule(&scenario), <[Fault]>::to_vec);
    let mut agent = LlmAgent::new(backend, max_turns);
    let result = run(&scenario, &schedule, &mut agent);
    if let Some(message) = agent.failure {
        return Err(format!("seed {seed}: {message}"));
    }
    let transcript = Transcript {
        model: model.to_string(),
        system: SYSTEM_PROMPT.to_string(),
        max_turns,
        turn_cap_hits: agent.turn_cap_hits,
        exchanges: agent.exchanges,
    };
    Ok((result, transcript, agent.backend))
}

/// Re-runs a recorded LLM repro from its transcript. Never calls a model.
pub fn replay(
    kind: ScenarioKind,
    seed: u64,
    faults: &[Fault],
    recorded: &Transcript,
) -> Result<RunResult, String> {
    if recorded.system != SYSTEM_PROMPT {
        return Err("transcript mismatch: system prompt differs from the recording".to_string());
    }
    let backend = Replay::new(recorded.exchanges.clone());
    let (result, transcript, backend) = run_model(
        kind,
        seed,
        Some(faults),
        backend,
        &recorded.model,
        recorded.max_turns,
    )?;
    if backend.remaining() > 0 {
        return Err(format!(
            "transcript mismatch: {} recorded exchanges were never requested",
            backend.remaining()
        ));
    }
    if transcript.turn_cap_hits != recorded.turn_cap_hits {
        return Err("transcript mismatch: turn cap hits differ".to_string());
    }
    Ok(result)
}
