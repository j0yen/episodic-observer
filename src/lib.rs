//! episodic-observer — end-of-session JSONL detectors that surface candidate
//! `episodic` memories from a Claude Code session journal.
//!
//! See PRD-episodic-observer.md. Phase 0 detectors:
//!   - user-redirect
//!   - revert
//!   - retry-with-tweak

#![cfg_attr(not(test), forbid(unsafe_code))]
#![allow(
    clippy::module_name_repetitions,
    clippy::trivially_copy_pass_by_ref,
    clippy::option_if_let_else,
    clippy::single_match_else,
    clippy::doc_markdown,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::unnecessary_wraps,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::float_arithmetic,
    clippy::as_conversions,
    clippy::or_fun_call,
    clippy::uninlined_format_args
)]

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Available detector names.
pub const DETECTORS: &[&str] = &["retry-with-tweak", "revert", "user-redirect"];

/// Redirect keywords (lowercased) that trigger `user-redirect`.
const REDIRECT_KEYWORDS: &[&str] = &["no", "stop", "don't", "dont", "actually", "wait"];

/// Default cap on candidates emitted per session.
pub const DEFAULT_MAX_MEMORIES: usize = 5;

/// One candidate memory the writer (downstream) may persist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Name of the detector that produced this candidate.
    pub detector: String,
    /// Recall-style subject, typically `project:<basename>`.
    pub subject: String,
    /// Human-readable memory body.
    pub body: String,
    /// Evidence references back to the source session journal.
    pub evidence: Vec<EvidenceRef>,
}

/// One source citation in a candidate's `evidence` list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Session JSONL basename.
    pub session: String,
    /// 1-indexed turn number.
    pub turn: u32,
    /// Short verbatim excerpt from the source event.
    pub excerpt: String,
}

/// Normalized session event.
#[derive(Debug, Clone)]
pub struct Event {
    /// 1-indexed turn within the session.
    pub turn: u32,
    /// Event kind, parsed out of the raw JSONL line.
    pub kind: EventKind,
}

/// Concrete event variants the detectors recognize.
#[derive(Debug, Clone)]
pub enum EventKind {
    /// User message text.
    User {
        /// Lowercase trimmed message body for keyword scanning.
        text: String,
    },
    /// Assistant emitted a tool-use.
    ToolUse {
        /// Tool name (e.g. "Bash", "Edit", "Write").
        name: String,
        /// Tool input fields (path/command/etc.).
        input: serde_json::Value,
    },
    /// Result of a prior tool-use, possibly carrying an exit code.
    ToolResult {
        /// Best-effort exit code (None if the tool doesn't expose one).
        exit_code: Option<i32>,
        /// Whether the underlying tool flagged an error.
        is_error: bool,
        /// Which tool-use this corresponds to (turn index of the originating tool-use, if known).
        for_turn: Option<u32>,
    },
}

/// Parse the contents of a JSONL session journal into normalized events.
///
/// Best-effort: lines that don't parse to an object are skipped (with a warning to stderr if caller emits one).
/// Returns the events in input order, with auto-assigned turns starting at 1.
#[must_use]
pub fn parse_session(jsonl: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut turn: u32 = 0;
    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        turn += 1;
        if let Some(e) = classify_event(turn, &v) {
            events.push(Event { turn, kind: e });
        }
    }
    events
}

fn classify_event(_turn: u32, v: &serde_json::Value) -> Option<EventKind> {
    let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
    match ty {
        "user" => {
            let text = v.get("text").and_then(|s| s.as_str())?.to_string();
            Some(EventKind::User { text })
        }
        "assistant" | "tool_use" => {
            let tu = v.get("tool_use").unwrap_or(v);
            let name = tu.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                return None;
            }
            let input = tu.get("input").cloned().unwrap_or(serde_json::json!({}));
            Some(EventKind::ToolUse { name, input })
        }
        "tool_result" => {
            let exit_code = v.get("exit_code").and_then(serde_json::Value::as_i64).and_then(|n| i32::try_from(n).ok());
            let is_error = v.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
            let for_turn = v.get("for_turn").and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok());
            Some(EventKind::ToolResult { exit_code, is_error, for_turn })
        }
        _ => None,
    }
}

/// Run all enabled detectors against the parsed events. Caps emission at `max`.
#[must_use]
pub fn observe(events: &[Event], session: &str, subject: &str, max: usize) -> Vec<Candidate> {
    let mut all = Vec::new();
    all.extend(detect_user_redirect(events, session, subject));
    all.extend(detect_revert(events, session, subject));
    all.extend(detect_retry_with_tweak(events, session, subject));
    // Deterministic order: by detector name then first evidence turn.
    all.sort_by(|a, b| {
        a.detector
            .cmp(&b.detector)
            .then_with(|| first_turn(a).cmp(&first_turn(b)))
    });
    all.truncate(max);
    all
}

fn first_turn(c: &Candidate) -> u32 {
    c.evidence.first().map_or(0, |e| e.turn)
}

fn detect_user_redirect(events: &[Event], session: &str, subject: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (i, e) in events.iter().enumerate() {
        let EventKind::User { text } = &e.kind else { continue };
        let lower = text.to_lowercase();
        let trimmed = lower.trim_start_matches(|c: char| !c.is_alphabetic());
        let starts_redirect = REDIRECT_KEYWORDS.iter().any(|k| {
            let after = trimmed.strip_prefix(k);
            matches!(after, Some(rest) if rest.starts_with(|c: char| !c.is_alphabetic()) || rest.is_empty())
        });
        if !starts_redirect {
            continue;
        }
        // Find any prior tool-use within 2 turns.
        let window_start = e.turn.saturating_sub(2);
        let mut prior_idx = None;
        for j in (0..i).rev() {
            if events[j].turn < window_start {
                break;
            }
            if matches!(events[j].kind, EventKind::ToolUse { .. }) {
                prior_idx = Some(j);
                break;
            }
        }
        let Some(j) = prior_idx else { continue };
        let prior_excerpt = match &events[j].kind {
            EventKind::ToolUse { name, .. } => format!("[{name}]"),
            _ => String::new(),
        };
        out.push(Candidate {
            detector: "user-redirect".to_string(),
            subject: subject.to_string(),
            body: format!(
                "User said \"{}\" — redirected away from prior {} tool-use.",
                text.trim(),
                prior_excerpt
            ),
            evidence: vec![
                EvidenceRef {
                    session: session.to_string(),
                    turn: events[j].turn,
                    excerpt: prior_excerpt,
                },
                EvidenceRef {
                    session: session.to_string(),
                    turn: e.turn,
                    excerpt: text.trim().to_string(),
                },
            ],
        });
    }
    out
}

fn detect_revert(events: &[Event], session: &str, subject: &str) -> Vec<Candidate> {
    // Track each Edit/Write tool-use: (turn, path, old_string, new_string).
    #[derive(Debug, Clone)]
    struct Mut {
        turn: u32,
        path: String,
        old: Option<String>,
        new: Option<String>,
    }
    let mut muts: Vec<Mut> = Vec::new();
    for e in events {
        if let EventKind::ToolUse { name, input } = &e.kind {
            if name == "Edit" || name == "Write" {
                muts.push(Mut {
                    turn: e.turn,
                    path: input.get("path").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                    old: input.get("old_string").and_then(|s| s.as_str()).map(String::from),
                    new: input.get("new_string").and_then(|s| s.as_str()).map(String::from),
                });
            }
        }
    }
    let mut out = Vec::new();
    let mut used: HashSet<u32> = HashSet::new();
    for (i, a) in muts.iter().enumerate() {
        if used.contains(&a.turn) {
            continue;
        }
        for b in muts.iter().skip(i + 1) {
            if b.turn.saturating_sub(a.turn) > 5 || a.path != b.path || a.path.is_empty() {
                continue;
            }
            // Textual revert: a.new == b.old and b.new == a.old.
            let reverts = match (&a.new, &b.old, &b.new, &a.old) {
                (Some(an), Some(bo), Some(bn), Some(ao)) => an == bo && bn == ao,
                _ => false,
            };
            if !reverts {
                continue;
            }
            used.insert(a.turn);
            used.insert(b.turn);
            out.push(Candidate {
                detector: "revert".to_string(),
                subject: subject.to_string(),
                body: format!(
                    "Edited `{}` at turn {} and reverted at turn {} (within {} turns).",
                    a.path,
                    a.turn,
                    b.turn,
                    b.turn - a.turn
                ),
                evidence: vec![
                    EvidenceRef {
                        session: session.to_string(),
                        turn: a.turn,
                        excerpt: format!("Edit {} (apply)", a.path),
                    },
                    EvidenceRef {
                        session: session.to_string(),
                        turn: b.turn,
                        excerpt: format!("Edit {} (revert)", b.path),
                    },
                ],
            });
            break;
        }
    }
    out
}

fn detect_retry_with_tweak(events: &[Event], session: &str, subject: &str) -> Vec<Candidate> {
    // Build pairs of (Bash turn, command, exit_code).
    let mut bashes: Vec<(u32, String, Option<i32>, bool)> = Vec::new();
    // First pass: collect Bash tool-uses.
    let mut last_bash_turn: HashMap<u32, usize> = HashMap::new();
    for e in events {
        if let EventKind::ToolUse { name, input } = &e.kind {
            if name == "Bash" {
                let cmd = input.get("command").and_then(|s| s.as_str()).unwrap_or("").to_string();
                last_bash_turn.insert(e.turn, bashes.len());
                bashes.push((e.turn, cmd, None, false));
            }
        }
    }
    // Second pass: pair tool_results with their Bash tool-uses by for_turn.
    for e in events {
        if let EventKind::ToolResult { exit_code, is_error, for_turn } = &e.kind {
            let Some(ft) = for_turn else { continue };
            if let Some(idx) = last_bash_turn.get(ft) {
                if let Some(slot) = bashes.get_mut(*idx) {
                    slot.2 = *exit_code;
                    slot.3 = *is_error;
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut used: HashSet<u32> = HashSet::new();
    for i in 0..bashes.len() {
        let (ai_turn, ai_cmd, ai_exit, ai_err) = bashes[i].clone();
        let ai_failed = ai_exit.is_some_and(|c| c != 0) || ai_err;
        if !ai_failed || used.contains(&ai_turn) {
            continue;
        }
        for j in i + 1..bashes.len() {
            let (bj_turn, bj_cmd, bj_exit, bj_err) = bashes[j].clone();
            if bj_turn.saturating_sub(ai_turn) > 3 {
                break;
            }
            let bj_ok = bj_exit == Some(0) && !bj_err;
            if !bj_ok {
                continue;
            }
            if jaccard_tokens(&ai_cmd, &bj_cmd) < 0.5 {
                continue;
            }
            used.insert(ai_turn);
            used.insert(bj_turn);
            out.push(Candidate {
                detector: "retry-with-tweak".to_string(),
                subject: subject.to_string(),
                body: format!(
                    "Bash `{}` failed at turn {}; retry `{}` succeeded at turn {}.",
                    ai_cmd, ai_turn, bj_cmd, bj_turn
                ),
                evidence: vec![
                    EvidenceRef {
                        session: session.to_string(),
                        turn: ai_turn,
                        excerpt: format!("$ {ai_cmd}"),
                    },
                    EvidenceRef {
                        session: session.to_string(),
                        turn: bj_turn,
                        excerpt: format!("$ {bj_cmd}"),
                    },
                ],
            });
            break;
        }
    }
    out
}

fn jaccard_tokens(a: &str, b: &str) -> f64 {
    let ta: HashSet<&str> = a.split_whitespace().collect();
    let tb: HashSet<&str> = b.split_whitespace().collect();
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let i = inter as f64;
        #[allow(clippy::cast_precision_loss)]
        let u = union as f64;
        i / u
    }
}
