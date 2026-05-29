# Changelog

## v0.2.0 — 2026-05-29

Add `episode observe-cross` for cross-session episodic pattern detection.
Detects corrective (session A errors on path P, session B fixes P within
300s), redundant (overlapping edits to same file within 1h, no claim), and
rescue (stalled session A, session B picks up its slug/paths) patterns
across multiple session transcripts plus the agorabus event log. Emits
`episodic-cross-session` candidates with a `participants` list and writes
recall proposals under `--write-proposals`. Strict windows + same-path
filters keep false positives low. Single-session `episode observe` is
unchanged.
