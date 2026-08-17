//! Structured, versioned requirement specifications and repository intake analysis.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{OrchestratorError, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementSpecification {
    pub goal: String,
    pub background: String,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub constraints: Vec<String>,
    pub assumptions: Vec<String>,
    pub user_scenarios: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub risks: Vec<String>,
    pub open_questions: Vec<String>,
}

impl RequirementSpecification {
    pub fn validate(&self) -> Result<()> {
        let required = [
            ("goal", !self.goal.trim().is_empty()),
            ("background", !self.background.trim().is_empty()),
            ("in_scope", !self.in_scope.is_empty()),
            ("acceptance_criteria", !self.acceptance_criteria.is_empty()),
        ];
        let missing: Vec<_> = required
            .into_iter()
            .filter_map(|(name, present)| (!present).then_some(name))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(OrchestratorError::InvalidInput(format!(
                "specification is missing required content: {}",
                missing.join(", ")
            )))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPriority {
    Architecture,
    Acceptance,
    Delivery,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalysisQuestion {
    /// Stable semantic key used to merge equivalent questions from multiple signals.
    pub key: String,
    pub question: String,
    pub priority: QuestionPriority,
    pub rationale: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryAnalysis {
    pub known_facts: Vec<String>,
    pub reasonable_inferences: Vec<String>,
    pub questions_for_user: Vec<AnalysisQuestion>,
}

/// Performs read-only intake analysis. It deliberately reports observations separately from
/// inference, and de-duplicates questions by semantic key before priority ordering.
pub fn analyze_repository(repository: impl AsRef<Path>) -> Result<RepositoryAnalysis> {
    let root = repository.as_ref();
    if !root.is_dir() {
        return Err(OrchestratorError::InvalidInput(
            "repository path is not a directory".into(),
        ));
    }
    let mut facts = Vec::new();
    let mut inferences = Vec::new();
    let mut questions = Vec::new();
    let has = |name: &str| root.join(name).exists();

    if has("Cargo.toml") {
        facts.push("Cargo.toml is present (Rust project or workspace).".into());
    }
    if has("package.json") {
        facts.push("package.json is present (Node.js package or workspace).".into());
    }
    if has(".github/workflows") {
        facts.push("GitHub Actions workflow configuration is present.".into());
    }
    if has("README.md") {
        facts.push("README.md is present.".into());
    }
    if has("Cargo.toml") && has("package.json") {
        inferences
            .push("Changes may need validation in both Rust and TypeScript toolchains.".into());
    }
    questions.push(AnalysisQuestion {
        key: "architecture-boundary".into(),
        question: "Which component owns this behavior and may public interfaces or storage schemas change?".into(),
        priority: QuestionPriority::Architecture,
        rationale: "The answer changes component boundaries and migration design.".into(),
    });
    questions.push(AnalysisQuestion {
        key: "acceptance-evidence".into(),
        question: "What observable outcomes and test evidence are required for acceptance?".into(),
        priority: QuestionPriority::Acceptance,
        rationale: "The answer determines completion and final verification.".into(),
    });
    questions.push(AnalysisQuestion {
        key: "compatibility".into(),
        question:
            "What backward-compatibility, platform, security, or performance constraints apply?"
                .into(),
        priority: QuestionPriority::Architecture,
        rationale: "These constraints can rule out implementation approaches.".into(),
    });
    questions.push(AnalysisQuestion {
        key: "delivery".into(),
        question: "Are there delivery milestones or explicitly excluded follow-up items?".into(),
        priority: QuestionPriority::Delivery,
        rationale: "This clarifies sequencing without blocking core architecture.".into(),
    });
    merge_and_sort_questions(&mut questions);
    Ok(RepositoryAnalysis {
        known_facts: facts,
        reasonable_inferences: inferences,
        questions_for_user: questions,
    })
}

fn merge_and_sort_questions(questions: &mut Vec<AnalysisQuestion>) {
    let mut keys = BTreeSet::new();
    questions.retain(|question| keys.insert(question.key.clone()));
    questions.sort_by_key(|question| match question.priority {
        QuestionPriority::Architecture => 0,
        QuestionPriority::Acceptance => 1,
        QuestionPriority::Delivery => 2,
    });
}

pub(crate) fn specification_markdown(
    requirement_id: uuid::Uuid,
    version: i64,
    specification: &RequirementSpecification,
) -> String {
    fn section(output: &mut String, title: &str, values: &[String]) {
        output.push_str(&format!("## {title}\n\n"));
        if values.is_empty() {
            output.push_str("- None.\n\n");
        } else {
            for value in values {
                output.push_str(&format!("- {}\n", value.trim()));
            }
            output.push('\n');
        }
    }
    let mut output = format!(
        "# Requirement Specification\n\n- Requirement: `{requirement_id}`\n- Version: `{version}`\n- Status: draft\n\n## Goal\n\n{}\n\n## Background\n\n{}\n\n",
        specification.goal.trim(), specification.background.trim()
    );
    section(&mut output, "In Scope", &specification.in_scope);
    section(&mut output, "Out of Scope", &specification.out_of_scope);
    section(&mut output, "Constraints", &specification.constraints);
    section(&mut output, "Assumptions", &specification.assumptions);
    section(&mut output, "User Scenarios", &specification.user_scenarios);
    section(
        &mut output,
        "Acceptance Criteria",
        &specification.acceptance_criteria,
    );
    section(&mut output, "Risks", &specification.risks);
    section(&mut output, "Open Questions", &specification.open_questions);
    output
}

pub(crate) fn write_specification(
    project_root: &Path,
    requirement_id: uuid::Uuid,
    version: i64,
    markdown: &str,
) -> Result<PathBuf> {
    let relative = PathBuf::from(".agentharbor/specs")
        .join(requirement_id.to_string())
        .join(format!("v{version}.md"));
    let path = project_root.join(&relative);
    let parent = path.parent().expect("specification path has a parent");
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("md.tmp");
    fs::write(&temporary, markdown)?;
    fs::rename(temporary, &path)?;
    Ok(relative)
}
