# episodic-observer

> Recall ships an `episodic` memory kind, but nothing populates it.

## Why

Recall ships an `episodic` memory kind, but nothing populates it. I write semantic+reflective memories explicitly; episodic patterns (try/fail/retry, user-redirect, tool-thrash) never land because they require an *observer*, not an author. This slice ships an end-of-session JSONL detector that surfaces the loadbearing patterns and emits candidate memories. Stop-hook integration + recall-write are downstream; this slice ends at `episode observe --dry-run <jsonl>` producing well-typed candidates a downstream writer can consume.

## Build

```sh
cargo build --release
```

Produces `target/release/episode`. Symlink into `~/.local/bin/` if you want it on `$PATH`.

## Usage

```sh
episode --help
```

## Audience

Future Claude sessions on this laptop, invoked from a Stop hook after a session ends. Also: the author manually testing detectors via `episode observe --dry-run <session.jsonl>` to see what would land in recall before wiring the hook. Pure CLI; no daemon.

## Acceptance criteria

This project was scaffolded from a PRD via the `autobuilder` pipeline. The MUST-level acceptance criteria are:

- **AC1**: CLI binary `episode` accepts subcommand `observe <jsonl-path>` and parses a fixture JSONL stream of session events without panicking. Exit 0 when the fixture has no detector matches; emit nothing to stdout.
- **AC2**: `episode observe --dry-run <jsonl-path>` emits a JSON array of candidate memories to stdout, each with `{detector, subject, body, evidence: [{session, turn, excerpt}]}`. Without `--dry-run` the CLI emits nothing on stdout (writing is dow...
- **AC3**: `episode detectors` lists the available detector names to stdout, one per line. At minimum: `revert`, `user-redirect`, `retry-with-tweak`. Exit 0.
- **AC4**: The `user-redirect` detector fires when a user-role message starts with a redirect keyword (one of: `no`, `stop`, `don't`, `dont`, `actually`, `wait`) within 2 turns of an assistant-role tool-use. Produces a candidate with `detector: "us...
- **AC5**: The `revert` detector fires when an Edit or Write tool-use is followed within 5 turns by another Edit/Write that returns the same path's content to its pre-change state (textual revert). Produces a candidate with `detector: "revert"`, bo...
- **AC6**: The `retry-with-tweak` detector fires when a Bash tool-use exits non-zero, followed within 3 turns by another Bash tool-use with a similar command (Jaccard token similarity ≥ 0.5 ignoring whitespace) that exits zero. Produces a candidate...
- **AC7**: `--max-memories <N>` caps the total number of emitted candidates across all detectors. Default cap is 5. When the cap is reached the CLI stops emitting and exits 0; surplus matches are dropped silently.
- **AC8**: Each emitted candidate's `subject` field is derived as `project:<basename of cwd>` when run from a project dir, falling back to `project:unknown` when cwd is `/` or unset. Subject is included in JSON output and is deterministic per-fixture.
- **AC9**: Invocation errors (missing JSONL path, unreadable file, malformed CLI args) → exit code 2 with a single-line diagnostic on stderr. Malformed JSONL lines mid-stream → skip the line, log to stderr, continue.

Each AC has a matching integration test under `tests/acceptance_ac<n>.rs`.

## Provenance

Built via the [`autobuilder`](https://github.com/j0yen/autobuilder) pipeline (PRD intake -> intent-card -> scaffold -> iterate-and-prove). Originally consolidated as a subdir of the [`wintermute`](https://github.com/j0yen/wintermute) monorepo; this standalone repo is a fresh-init snapshot for easier consumption and distribution.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
