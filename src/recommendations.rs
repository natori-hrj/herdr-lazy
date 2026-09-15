//! Curated recommendations for the agent focused in the current Herdr workspace.
//!
//! Agent names come from Herdr, but the mapping belongs here rather than in the UI so it is
//! explicit, reviewable, and testable. An unknown agent deliberately produces no recommendation:
//! recognizing a name is a presentation hint, not a reason to guess at plugin identity.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Agent {
    Codex,
    ClaudeCode,
    Muse,
}

impl Agent {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::ClaudeCode => "Claude Code",
            Agent::Muse => "Muse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Recommendation {
    pub(crate) extra_id: &'static str,
    pub(crate) reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentRecommendations {
    pub(crate) agent: Agent,
    pub(crate) items: &'static [Recommendation],
}

const CODEX: &[Recommendation] = &[Recommendation {
    extra_id: "worktrunk",
    reason: "Keep each Codex task in an isolated git worktree.",
}];

const CLAUDE_CODE: &[Recommendation] = &[Recommendation {
    extra_id: "worktrunk",
    reason: "Keep each Claude Code task in an isolated git worktree.",
}];

const MUSE: &[Recommendation] = &[Recommendation {
    extra_id: "pluck",
    reason: "Capture paths, commits, and URLs from the focused Muse pane.",
}];

/// Return only recommendations for agent names this project deliberately understands.
pub(crate) fn for_agent(agent: &str) -> Option<AgentRecommendations> {
    let normalized = normalize(agent);
    let (agent, items) = match normalized.as_str() {
        "codex" | "openaicodex" | "codexcli" => (Agent::Codex, CODEX),
        "claudecode" | "anthropicclaudecode" => (Agent::ClaudeCode, CLAUDE_CODE),
        "muse" => (Agent::Muse, MUSE),
        _ => return None,
    };
    Some(AgentRecommendations { agent, items })
}

fn normalize(agent: &str) -> String {
    agent
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_curated_agent_aliases() {
        assert_eq!(for_agent("Codex").unwrap().agent, Agent::Codex);
        assert_eq!(for_agent("openai-codex").unwrap().agent, Agent::Codex);
        assert_eq!(for_agent("claude-code").unwrap().agent, Agent::ClaudeCode);
        assert_eq!(for_agent("Muse").unwrap().agent, Agent::Muse);
    }

    #[test]
    fn unknown_agents_are_not_guessed() {
        assert!(for_agent("some-new-agent").is_none());
        assert!(for_agent("").is_none());
    }

    #[test]
    fn every_curated_mapping_has_a_reason_and_extra() {
        for name in ["codex", "claude code", "muse"] {
            let recommendations = for_agent(name).expect("known agent");
            assert!(!recommendations.items.is_empty());
            for item in recommendations.items {
                assert!(!item.extra_id.is_empty());
                assert!(!item.reason.is_empty());
            }
        }
    }
}
