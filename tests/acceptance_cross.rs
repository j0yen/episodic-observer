//! Acceptance tests for PRD-chord-cross-episode iter-1 — Detector 1
//! (cross-session corrective).
//!
//! Covers AC1 (fires on synthetic data), AC2 (no false positive on
//! unrelated paths), and AC3 (300s window enforced). Detectors 2/3,
//! `--write-proposals` (AC9), and claim-suppression (AC5) arrive in
//! later iterations.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::doc_markdown)]

use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// Write `content` to `<dir>/<stem>.jsonl`.
fn session(dir: &std::path::Path, stem: &str, content: &str) {
    fs::write(dir.join(format!("{stem}.jsonl")), content).unwrap();
}

/// Run `episode observe-cross --since 1h --transcripts-dir <dir> --dry-run`
/// and return the parsed JSON array of candidates.
fn run_cross(dir: &std::path::Path) -> serde_json::Value {
    let bin = env!("CARGO_BIN_EXE_episode");
    let out = Command::new(bin)
        .args(["observe-cross", "--since", "1h", "--dry-run", "--transcripts-dir"])
        .arg(dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "observe-cross must exit 0");
    serde_json::from_slice(&out.stdout).unwrap()
}

fn err_line(ts: i64, path: &str) -> String {
    format!(r#"{{"type":"tool_use","ts":{ts},"tool_use":{{"name":"Bash","input":{{"path":"{path}"}}}},"is_error":true}}"#)
}

fn write_line(ts: i64, path: &str) -> String {
    format!(r#"{{"type":"tool_use","ts":{ts},"tool_use":{{"name":"Edit","input":{{"path":"{path}"}}}},"is_error":false}}"#)
}

#[test]
fn ac1_corrective_fires_on_synthetic_data() {
    let dir = tempdir().unwrap();
    session(dir.path(), "A", &format!("{}\n", err_line(1000, "/tmp/foo")));
    session(dir.path(), "B", &format!("{}\n", write_line(1030, "/tmp/foo")));

    let v = run_cross(dir.path());
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "exactly one cross-session candidate");
    let c = &arr[0];
    assert_eq!(c["kind"], "episodic-cross-session");
    assert_eq!(c["context"], "corrective");
    assert_eq!(c["participants"], serde_json::json!(["A", "B"]));
    assert_eq!(c["paths"], serde_json::json!(["/tmp/foo"]));
}

#[test]
fn ac2_no_false_positive_on_unrelated_paths() {
    let dir = tempdir().unwrap();
    session(dir.path(), "A", &format!("{}\n", err_line(1000, "/tmp/foo")));
    session(dir.path(), "B", &format!("{}\n", write_line(1030, "/tmp/bar")));

    let v = run_cross(dir.path());
    assert_eq!(v.as_array().unwrap().len(), 0, "unrelated paths must not correlate");
}

#[test]
fn ac3_window_enforced() {
    // 400s apart (> 300): no candidate.
    let far = tempdir().unwrap();
    session(far.path(), "A", &format!("{}\n", err_line(1000, "/tmp/foo")));
    session(far.path(), "B", &format!("{}\n", write_line(1400, "/tmp/foo")));
    assert_eq!(run_cross(far.path()).as_array().unwrap().len(), 0, "400s > window");

    // 290s apart (< 300): one candidate.
    let near = tempdir().unwrap();
    session(near.path(), "A", &format!("{}\n", err_line(1000, "/tmp/foo")));
    session(near.path(), "B", &format!("{}\n", write_line(1290, "/tmp/foo")));
    assert_eq!(run_cross(near.path()).as_array().unwrap().len(), 1, "290s < window");
}

/// Successful Edit carrying an explicit `[line_start, line_end]` range.
fn write_line_lr(ts: i64, path: &str, ls: i64, le: i64) -> String {
    format!(
        r#"{{"type":"tool_use","ts":{ts},"tool_use":{{"name":"Edit","input":{{"path":"{path}","line_start":{ls},"line_end":{le}}}}},"is_error":false}}"#
    )
}

/// Non-error, non-write activity (a Bash read) touching `path`.
fn act_line(ts: i64, path: &str) -> String {
    format!(r#"{{"type":"tool_use","ts":{ts},"tool_use":{{"name":"Bash","input":{{"path":"{path}"}}}},"is_error":false}}"#)
}

/// A session-intent declaration line (no path; sets the session slug).
fn intent_line(ts: i64, slug: &str) -> String {
    format!(r#"{{"type":"intent","ts":{ts},"intent_slug":"{slug}"}}"#)
}

#[test]
fn ac4_redundant_fires_on_overlapping_diffs() {
    // Two sessions edit the same file's same line range 20 min apart,
    // no claim present: exactly one `redundant` candidate.
    let dir = tempdir().unwrap();
    session(dir.path(), "A", &format!("{}\n", write_line_lr(1000, "/tmp/x", 10, 20)));
    session(dir.path(), "B", &format!("{}\n", write_line_lr(2200, "/tmp/x", 12, 18)));

    let v = run_cross(dir.path());
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "exactly one redundant candidate");
    let c = &arr[0];
    assert_eq!(c["kind"], "episodic-cross-session");
    assert_eq!(c["context"], "redundant");
    assert_eq!(c["participants"], serde_json::json!(["A", "B"]));
    assert_eq!(c["paths"], serde_json::json!(["/tmp/x"]));
}

#[test]
fn ac4_redundant_respects_window_and_overlap() {
    // Non-overlapping line ranges, same file, in window: no candidate.
    let no_overlap = tempdir().unwrap();
    session(no_overlap.path(), "A", &format!("{}\n", write_line_lr(1000, "/tmp/x", 1, 5)));
    session(no_overlap.path(), "B", &format!("{}\n", write_line_lr(1600, "/tmp/x", 40, 50)));
    assert_eq!(
        run_cross(no_overlap.path()).as_array().unwrap().len(),
        0,
        "disjoint line ranges must not be redundant"
    );
}

#[test]
fn ac6_rescue_fires_on_intent_named_rescue() {
    // A errors on /tmp/p then stalls (6 min of silence). B, whose intent
    // slug matches A's, resumes activity on /tmp/p within 10 min of the
    // stall: exactly one `rescue` candidate.
    let dir = tempdir().unwrap();
    session(
        dir.path(),
        "A",
        &format!("{}\n{}\n", intent_line(1000, "chord-x"), err_line(1000, "/tmp/p")),
    );
    session(
        dir.path(),
        "B",
        &format!("{}\n{}\n", intent_line(1360, "chord-x"), act_line(1360, "/tmp/p")),
    );

    let v = run_cross(dir.path());
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1, "exactly one rescue candidate");
    let c = &arr[0];
    assert_eq!(c["context"], "rescue");
    assert_eq!(c["participants"], serde_json::json!(["A", "B"]));
    assert_eq!(c["paths"], serde_json::json!(["/tmp/p"]));
}

#[test]
fn ac6_rescue_requires_matching_intent() {
    // Same shape as AC6 but B's intent slug differs: no candidate.
    let dir = tempdir().unwrap();
    session(
        dir.path(),
        "A",
        &format!("{}\n{}\n", intent_line(1000, "chord-x"), err_line(1000, "/tmp/p")),
    );
    session(
        dir.path(),
        "B",
        &format!("{}\n{}\n", intent_line(1360, "other-slug"), act_line(1360, "/tmp/p")),
    );
    assert_eq!(
        run_cross(dir.path()).as_array().unwrap().len(),
        0,
        "non-matching intent must not rescue"
    );
}

/// Run observe-cross with `--write-proposals` into an explicit dir.
fn run_cross_write(transcripts: &std::path::Path, proposals: &std::path::Path) {
    let bin = env!("CARGO_BIN_EXE_episode");
    let out = Command::new(bin)
        .args(["observe-cross", "--since", "1h", "--write-proposals", "--transcripts-dir"])
        .arg(transcripts)
        .arg("--proposals-dir")
        .arg(proposals)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "observe-cross --write-proposals must exit 0");
}

#[test]
fn ac9_write_proposals_lands_files_and_dry_run_does_not() {
    // One corrective candidate (A errors on /tmp/foo, B fixes 30s later).
    let transcripts = tempdir().unwrap();
    session(transcripts.path(), "A", &format!("{}\n", err_line(1000, "/tmp/foo")));
    session(transcripts.path(), "B", &format!("{}\n", write_line(1030, "/tmp/foo")));

    // --write-proposals writes exactly one .md proposal file.
    let proposals = tempdir().unwrap();
    run_cross_write(transcripts.path(), proposals.path());
    let written: Vec<_> = fs::read_dir(proposals.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    assert_eq!(written.len(), 1, "exactly one proposal file written");
    let body = fs::read_to_string(&written[0]).unwrap();
    assert!(body.starts_with("---\n"), "proposal has YAML frontmatter");
    assert!(body.contains("kind: episodic-cross-session"), "kind stamped");
    assert!(body.contains("context: corrective"), "context stamped");

    // --dry-run must write nothing, even with --write-proposals also passed.
    let bin = env!("CARGO_BIN_EXE_episode");
    let empty = tempdir().unwrap();
    let out = Command::new(bin)
        .args(["observe-cross", "--since", "1h", "--dry-run", "--write-proposals", "--transcripts-dir"])
        .arg(transcripts.path())
        .arg("--proposals-dir")
        .arg(empty.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let count = fs::read_dir(empty.path()).unwrap().count();
    assert_eq!(count, 0, "--dry-run must not write any proposal files");
}
