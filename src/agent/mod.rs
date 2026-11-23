//! The agent under test. Anything implementing `Agent` can run in the
//! simulation unchanged.

pub mod llm;
mod v1;
mod v2;

pub use v1::FirstPassAgent;
pub use v2::CarefulAgent;

use crate::model::Document;
use crate::tools::ToolPort;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

pub trait Agent {
    fn name(&self) -> &'static str;
    /// Handles one inbound document. All side effects go through `port`.
    fn handle(&mut self, doc: &Document, port: &mut dyn ToolPort);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    V1,
    V2,
    /// Model-driven. Runs only through the `llm` command or a recorded replay.
    #[value(skip)]
    Llm,
}

impl Policy {
    pub fn agent(self) -> Box<dyn Agent> {
        match self {
            Self::V1 => Box::<FirstPassAgent>::default(),
            Self::V2 => Box::<CarefulAgent>::default(),
            // With no transcript this agent makes no calls and records a failure.
            Self::Llm => Box::new(llm::LlmAgent::new(
                llm::Replay::new(Vec::new()),
                llm::DEFAULT_MAX_TURNS,
            )),
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
            Self::Llm => "llm",
        })
    }
}
