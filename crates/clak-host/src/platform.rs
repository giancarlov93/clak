//! The real system for the hub: what `clak-hub` asks for, answered by `clak-platform`.

use clak_hub::{AgentIdentity, Platform};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// The operating system the hub runs on.
pub struct SystemPlatform;

fn to_hub(agent: clak_platform::AgentIdentity) -> AgentIdentity {
    let started_at = agent
        .started_at()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64());
    AgentIdentity {
        pid: agent.pid,
        started: agent.started,
        started_at,
        exe: agent.exe,
        name: agent.name,
        chain: agent.chain,
        signed: agent.signed,
        signer: agent.signer,
    }
}

fn to_platform(agent: &AgentIdentity) -> clak_platform::AgentIdentity {
    clak_platform::AgentIdentity {
        pid: agent.pid,
        started: agent.started,
        exe: agent.exe.clone(),
        name: agent.name.clone(),
        chain: agent.chain.clone(),
        signed: agent.signed.clone(),
        signer: agent.signer.clone(),
    }
}

impl Platform for SystemPlatform {
    fn agent_of(
        &self,
        pid: Option<u32>,
        _request: &Map<String, Value>,
        authorized: &HashSet<String>,
    ) -> Option<AgentIdentity> {
        clak_platform::agent_of(pid, authorized).map(to_hub)
    }

    fn is_alive(&self, agent: &AgentIdentity) -> bool {
        clak_platform::is_alive(&to_platform(agent))
    }

    fn attest_loopback(&self, peer_port: u16, local_port: u16) -> Value {
        clak_platform::attest_loopback(peer_port, local_port)
    }

    fn executable_hash(&self, path: &str) -> Option<String> {
        clak_platform::executable_hash(path)
    }
}
