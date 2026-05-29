//! Cross-session episodic detectors.
//!
//! Where [`crate::observe`] looks at a single session JSONL, this module
//! correlates events *across* sessions modified within a time window:
//! an error in session A followed by a corrective write in session B.
//!
//! See `PRD-chord-cross-episode.md`. iter-1 ships **Detector 1
//! (corrective)** only; detectors 2 (redundant) and 3 (rescue),
//! `--write-proposals`, and agorabus claim-suppression land in later
//! iterations. The single-session path in [`crate::observe`] is
//! untouched — `observe-cross` is strictly additive.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Detector 1 corrective window: a fix must follow its error within this
/// many seconds (strict, exclusive on both ends — `0 < dt < WINDOW`).
pub const CORRECTIVE_WINDOW_SECS: i64 = 300;

/// Detector 2 redundant-work window: two sessions writing the same path
/// within this many seconds of each other are candidate-redundant
/// (strict, `0 < dt < WINDOW`).
pub const REDUNDANT_WINDOW_SECS: i64 = 3600;

/// Detector 3 stall threshold: session A must go quiet for at least this
/// many seconds after an error for the error to count as a stall.
pub const STALL_SECS: i64 = 300;

/// Detector 3 rescue window: the rescuing session must begin shared-path
/// activity within this many seconds *after the stall* (i.e. up to
/// `STALL_SECS + RESCUE_WINDOW_SECS` after the originating error).
pub const RESCUE_WINDOW_SECS: i64 = 600;

/// The `kind` stamped on every cross-session candidate.
pub const CROSS_KIND: &str = "episodic-cross-session";

/// One normalized, timestamped action drawn from a session JSONL.
///
/// A line contributes a record when it carries both a `path` and a unix
/// timestamp (`ts`). The action's tool name and error status decide which
/// detector inputs it feeds.
#[derive(Debug, Clone)]
pub struct CrossRecord {
    /// Unix timestamp (seconds) of the action.
    pub ts: i64,
    /// Filesystem path the action touched.
    pub path: String,
    /// Tool name (e.g. `Edit`, `Write`, `Bash`).
    pub tool: String,
    /// Whether the action's result flagged an error / non-zero exit.
    pub is_error: bool,
    /// First line of the edited range, when the tool call carried one.
    pub line_start: Option<i64>,
    /// Last line of the edited range, when the tool call carried one.
    pub line_end: Option<i64>,
}

/// True when two optional `[start, end]` line ranges overlap.
///
/// Degraded fallback: if *either* range is absent (the JSONL Edit schema
/// didn't carry line info — see PRD §Risks), the two are treated as
/// overlapping so detector 2 falls back to "same file within window."
#[must_use]
const fn line_ranges_overlap(
    a: (Option<i64>, Option<i64>),
    b: (Option<i64>, Option<i64>),
) -> bool {
    match (a, b) {
        ((Some(a0), Some(a1)), (Some(b0), Some(b1))) => a0 <= b1 && b0 <= a1,
        _ => true,
    }
}

impl CrossRecord {
    /// True when this record is a successful `Edit`/`Write` to its path.
    #[must_use]
    pub fn is_write_success(&self) -> bool {
        !self.is_error
            && !self.path.is_empty()
            && (self.tool == "Edit" || self.tool == "Write")
    }

    /// True when this record represents an error on its path.
    #[must_use]
    pub fn is_path_error(&self) -> bool {
        self.is_error && !self.path.is_empty()
    }
}

/// All records from one session, in timestamp order.
#[derive(Debug, Clone)]
pub struct Session {
    /// Stable session id (the JSONL file stem).
    pub sid: String,
    /// Records parsed from the session, sorted ascending by `ts`.
    pub records: Vec<CrossRecord>,
    /// Declared intent slug (skill/PRD the session named), if any. Drawn
    /// from an `intent_slug` field on any line; the last one wins.
    pub slug: Option<String>,
}

/// One source citation in a cross-session candidate's `evidence` list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossEvidence {
    /// Session id the cited event came from.
    pub session: String,
    /// Unix timestamp of the cited event.
    pub ts: i64,
    /// Short human-readable excerpt.
    pub excerpt: String,
}

/// A candidate cross-session episodic memory.
///
/// Schema mirror: `schema/episode-cross-session.schema.json` (AC8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCandidate {
    /// Always [`CROSS_KIND`].
    pub kind: String,
    /// Recall-style subject; `self` for agent-behavioral patterns.
    pub subject: String,
    /// The sessions involved, length >= 2, error-session first.
    pub participants: Vec<String>,
    /// Pattern context: `corrective` | `redundant` | `rescue`.
    pub context: String,
    /// Paths the pattern centers on.
    pub paths: Vec<String>,
    /// Earliest timestamp across the cited evidence.
    pub t_first_unix: i64,
    /// Latest timestamp across the cited evidence.
    pub t_last_unix: i64,
    /// Source citations.
    pub evidence: Vec<CrossEvidence>,
}

/// Parse a `since` duration string (e.g. `90s`, `30m`, `2h`, `1d`) into
/// seconds. A bare integer is interpreted as seconds. Returns `None` on a
/// malformed value.
#[must_use]
pub fn parse_since(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => return None,
    };
    num.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| n * mult)
}

/// Parse a single session JSONL into timestamped records.
///
/// Best-effort: lines that don't parse to an object, or that carry no
/// `path`/`ts`, are skipped. Records come back sorted ascending by `ts`.
#[must_use]
pub fn parse_cross_session(jsonl: &str) -> Vec<CrossRecord> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(ts) = v.get("ts").or_else(|| v.get("timestamp")).and_then(serde_json::Value::as_i64)
        else {
            continue;
        };
        // Tool name + path may live at the top level or under `tool_use`.
        let tu = v.get("tool_use").unwrap_or(&v);
        let tool = tu.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let path = tu
            .get("input")
            .and_then(|i| i.get("path"))
            .or_else(|| v.get("path"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if path.is_empty() {
            continue;
        }
        let input = tu.get("input");
        let line_start = input
            .and_then(|i| i.get("line_start"))
            .and_then(serde_json::Value::as_i64);
        let line_end = input
            .and_then(|i| i.get("line_end"))
            .and_then(serde_json::Value::as_i64);
        let exit_err = v.get("exit_code").and_then(serde_json::Value::as_i64).is_some_and(|c| c != 0);
        let flag_err = v.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false);
        out.push(CrossRecord {
            ts,
            path,
            tool,
            is_error: exit_err || flag_err,
            line_start,
            line_end,
        });
    }
    out.sort_by_key(|r| r.ts);
    out
}

/// Extract the session's declared intent slug from its JSONL, if present.
///
/// Scans every line for an `intent_slug` string; the last non-empty value
/// wins (a session's intent can reshape mid-run — the latest reshape is
/// what the rescuing session would have observed).
#[must_use]
pub fn parse_session_slug(jsonl: &str) -> Option<String> {
    let mut slug = None;
    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(s) = v.get("intent_slug").and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
            slug = Some(s.to_string());
        }
    }
    slug
}

/// Load every `*.jsonl` under `dir` whose mtime is within `since_secs` of
/// now, parse it, and return one [`Session`] per file (sorted by `sid`).
///
/// # Errors
/// Returns an error string if `dir` cannot be read.
pub fn load_recent_sessions(dir: &Path, since_secs: i64) -> Result<Vec<Session>, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let cutoff = now.saturating_sub(since_secs);

    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        if mtime < cutoff {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let sid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
        sessions.push(Session {
            sid,
            records: parse_cross_session(&content),
            slug: parse_session_slug(&content),
        });
    }
    sessions.sort_by(|a, b| a.sid.cmp(&b.sid));
    Ok(sessions)
}

/// Run the cross-session detectors over the loaded sessions.
///
/// iter-2: Detector 1 (corrective), Detector 2 (redundant), and
/// Detector 3 (rescue). iter-3 adds proposal-file output via
/// [`write_proposals`] (AC9). Claim-suppression for detector 2 (AC5)
/// stays deferred, gated on chord-claim shipping.
#[must_use]
pub fn observe_cross(sessions: &[Session]) -> Vec<CrossCandidate> {
    let mut out = detect_corrective(sessions, CORRECTIVE_WINDOW_SECS);
    out.extend(detect_redundant(sessions, REDUNDANT_WINDOW_SECS));
    out.extend(detect_rescue(sessions, STALL_SECS, RESCUE_WINDOW_SECS));
    out.sort_by(|a, b| {
        a.context
            .cmp(&b.context)
            .then_with(|| a.t_first_unix.cmp(&b.t_first_unix))
            .then_with(|| a.participants.cmp(&b.participants))
    });
    out
}

/// Detector 1 — cross-session corrective.
///
/// Session A errors on path `P` at `T_A`; session B (B != A) writes `P`
/// successfully at `T_B` with `0 < T_B - T_A < window`. The earliest such
/// fix per (error, fixing-session) pair is emitted once.
#[must_use]
pub fn detect_corrective(sessions: &[Session], window: i64) -> Vec<CrossCandidate> {
    let mut out = Vec::new();
    for a in sessions {
        for err in a.records.iter().filter(|r| r.is_path_error()) {
            for b in sessions {
                if b.sid == a.sid {
                    continue;
                }
                let fix = b
                    .records
                    .iter()
                    .filter(|w| w.is_write_success() && w.path == err.path)
                    .map(|w| (w.ts.saturating_sub(err.ts), w))
                    .filter(|(dt, _)| *dt > 0 && *dt < window)
                    .min_by_key(|(dt, _)| *dt);
                let Some((_, w)) = fix else { continue };
                out.push(CrossCandidate {
                    kind: CROSS_KIND.to_string(),
                    subject: "self".to_string(),
                    participants: vec![a.sid.clone(), b.sid.clone()],
                    context: "corrective".to_string(),
                    paths: vec![err.path.clone()],
                    t_first_unix: err.ts,
                    t_last_unix: w.ts,
                    evidence: vec![
                        CrossEvidence {
                            session: a.sid.clone(),
                            ts: err.ts,
                            excerpt: format!("error on {}", err.path),
                        },
                        CrossEvidence {
                            session: b.sid.clone(),
                            ts: w.ts,
                            excerpt: format!("{} {} (ok)", w.tool, w.path),
                        },
                    ],
                });
            }
        }
    }
    out
}

/// Detector 2 — redundant work.
///
/// Sessions A and B (distinct) both write the same path `P` successfully
/// within `0 < dt < window`, and their edited line ranges overlap (or
/// either range is absent — the degraded same-file fallback). Each
/// unordered session pair emits at most one candidate per path, using the
/// earliest write in each session; participants are ordered earliest-write
/// first.
///
/// AC5 claim-suppression (skip when an agorabus `claim.acquire` spans the
/// window) is deferred to iter-3, gated on chord-claim shipping.
#[must_use]
pub fn detect_redundant(sessions: &[Session], window: i64) -> Vec<CrossCandidate> {
    let mut out = Vec::new();
    for (i, a) in sessions.iter().enumerate() {
        for b in &sessions[i + 1..] {
            // Collect the paths each session wrote, keeping the earliest
            // successful write per path.
            let earliest_write = |s: &Session, p: &str| -> Option<CrossRecord> {
                s.records
                    .iter()
                    .filter(|r| r.is_write_success() && r.path == p)
                    .min_by_key(|r| r.ts)
                    .cloned()
            };
            let mut paths: Vec<String> = a
                .records
                .iter()
                .filter(|r| r.is_write_success())
                .map(|r| r.path.clone())
                .collect();
            paths.sort();
            paths.dedup();
            for p in paths {
                let (Some(wa), Some(wb)) = (earliest_write(a, &p), earliest_write(b, &p)) else {
                    continue;
                };
                let dt = (wa.ts - wb.ts).abs();
                if dt == 0 || dt >= window {
                    continue;
                }
                if !line_ranges_overlap((wa.line_start, wa.line_end), (wb.line_start, wb.line_end)) {
                    continue;
                }
                // Earliest writer first.
                let (first, second) = if wa.ts <= wb.ts { (a, b) } else { (b, a) };
                let (fw, sw) = if wa.ts <= wb.ts { (&wa, &wb) } else { (&wb, &wa) };
                out.push(CrossCandidate {
                    kind: CROSS_KIND.to_string(),
                    subject: "self".to_string(),
                    participants: vec![first.sid.clone(), second.sid.clone()],
                    context: "redundant".to_string(),
                    paths: vec![p.clone()],
                    t_first_unix: fw.ts,
                    t_last_unix: sw.ts,
                    evidence: vec![
                        CrossEvidence {
                            session: first.sid.clone(),
                            ts: fw.ts,
                            excerpt: format!("{} {} (ok)", fw.tool, fw.path),
                        },
                        CrossEvidence {
                            session: second.sid.clone(),
                            ts: sw.ts,
                            excerpt: format!("{} {} (ok, overlapping)", sw.tool, sw.path),
                        },
                    ],
                });
            }
        }
    }
    out
}

/// Detector 3 — rescue (lowest precision; see PRD §Risks).
///
/// Session A errors on a path and then stalls (no further A activity for
/// at least `stall` seconds). Session B, whose declared intent slug
/// matches A's, begins activity on a path A also touched within
/// `stall + rescue` seconds of A's error. Emits one `rescue` candidate per
/// (error, rescuing-session) pair, participants `[A, B]`.
#[must_use]
pub fn detect_rescue(sessions: &[Session], stall: i64, rescue: i64) -> Vec<CrossCandidate> {
    let mut out = Vec::new();
    for a in sessions {
        let a_paths: std::collections::HashSet<&str> =
            a.records.iter().map(|r| r.path.as_str()).collect();
        for err in a.records.iter().filter(|r| r.is_path_error()) {
            // Stall: no A record in (err.ts, err.ts + stall].
            let stalled = !a
                .records
                .iter()
                .any(|r| r.ts > err.ts && r.ts <= err.ts + stall);
            if !stalled {
                continue;
            }
            for b in sessions {
                if b.sid == a.sid {
                    continue;
                }
                // Intent must be declared on both and match.
                if b.slug.is_none() || b.slug != a.slug {
                    continue;
                }
                // B's earliest shared-path activity after the error, within
                // the stall+rescue window.
                let onset = b
                    .records
                    .iter()
                    .filter(|r| a_paths.contains(r.path.as_str()))
                    .map(|r| (r.ts.saturating_sub(err.ts), r))
                    .filter(|(dt, _)| *dt > 0 && *dt <= stall + rescue)
                    .min_by_key(|(dt, _)| *dt);
                let Some((_, r)) = onset else { continue };
                out.push(CrossCandidate {
                    kind: CROSS_KIND.to_string(),
                    subject: "self".to_string(),
                    participants: vec![a.sid.clone(), b.sid.clone()],
                    context: "rescue".to_string(),
                    paths: vec![r.path.clone()],
                    t_first_unix: err.ts,
                    t_last_unix: r.ts,
                    evidence: vec![
                        CrossEvidence {
                            session: a.sid.clone(),
                            ts: err.ts,
                            excerpt: format!("error then stall on {}", err.path),
                        },
                        CrossEvidence {
                            session: b.sid.clone(),
                            ts: r.ts,
                            excerpt: format!(
                                "intent {} resumed {} on {}",
                                b.slug.as_deref().unwrap_or(""),
                                r.tool,
                                r.path
                            ),
                        },
                    ],
                });
            }
        }
    }
    out
}

/// Per-detector confidence stamped on a written proposal. Mirrors the
/// PRD's stated precision ordering: corrective > redundant > rescue.
#[must_use]
fn confidence_for(context: &str) -> f64 {
    match context {
        "corrective" => 0.6,
        "redundant" => 0.5,
        // rescue is explicitly the lowest-precision detector (PRD §Risks).
        _ => 0.3,
    }
}

/// Sanitize a session id into a filename-safe token (ASCII alphanumerics
/// plus `-`/`_` kept; anything else becomes `_`).
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Deterministic proposal filename stem for a candidate.
///
/// Content-derived (context + window + participants) so re-running
/// `observe-cross --write-proposals` over the same window overwrites the
/// same file rather than accumulating duplicates.
#[must_use]
pub fn proposal_stem(c: &CrossCandidate) -> String {
    let parts = c.participants.iter().map(|p| sanitize(p)).collect::<Vec<_>>().join("+");
    format!("cross-{}-{}-{}-{}", c.context, c.t_first_unix, c.t_last_unix, parts)
}

/// Render a candidate as a recall proposal file (YAML frontmatter +
/// markdown body), mirroring the recall-observer-correlation convention
/// under `~/.claude/recall/proposals/`.
#[must_use]
pub fn render_proposal(c: &CrossCandidate, created_unix: i64) -> String {
    let stem = proposal_stem(c);
    let participants = c.participants.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n");
    let paths = c.paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n");
    let evidence = c
        .evidence
        .iter()
        .map(|e| format!("- `{}` @ {}: {}", e.session, e.ts, e.excerpt))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "---\nid: {stem}\nkind: {kind}\nsubject: {subject}\ncontext: {context}\nparticipants:\n{participants}\npaths:\n{paths}\nt_first_unix: {tf}\nt_last_unix: {tl}\ncreated_unix: {created_unix}\nconfidence: {conf}\nrecall_count: 0\n---\n\nCross-session {context} pattern across sessions {plist}.\n\n{evidence}\n\n<!-- episode observe-cross: detector={context} -->\n",
        kind = c.kind,
        subject = c.subject,
        context = c.context,
        tf = c.t_first_unix,
        tl = c.t_last_unix,
        conf = confidence_for(&c.context),
        plist = c.participants.join(", "),
    )
}

/// Write each candidate as a proposal file under `dir`.
///
/// Creates `dir` (and parents) if needed. Filenames are
/// [`proposal_stem`]-derived, so re-runs overwrite rather than duplicate.
/// Returns the paths written.
///
/// # Errors
/// Returns an error string if `dir` cannot be created or a file cannot be
/// written.
pub fn write_proposals(
    candidates: &[CrossCandidate],
    dir: &Path,
    created_unix: i64,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut written = Vec::with_capacity(candidates.len());
    for c in candidates {
        let path = dir.join(format!("{}.md", proposal_stem(c)));
        std::fs::write(&path, render_proposal(c, created_unix))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Current unix time in seconds, saturating; `0` if the clock is before
/// the epoch. Used to stamp `created_unix` on written proposals.
#[must_use]
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
