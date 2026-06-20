# episodic-observer

`episode` reads Claude session transcripts after the fact and surfaces the patterns worth remembering — a try/fail/retry, a user redirect, a reverted edit — as candidate episodic memories for recall to keep.

## Why it exists

Recall ships an `episodic` memory kind, but nothing fills it. Semantic and reflective memories get written deliberately, because the session knows it learned something. Episodic patterns don't work that way: the moment you correct course or undo a change, you're busy doing the next thing, not narrating it. Catching those moments needs an observer reading the transcript, not an author writing as it happens. `episode` is that observer. It runs on the JSONL a session leaves behind and proposes the candidates a downstream writer can persist.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/j0yen/episodic-observer/main/install.sh | bash
```

Or build it yourself — requires `cargo` and `rustc 1.85+`:

```sh
git clone --depth 1 https://github.com/j0yen/episodic-observer.git
cd episodic-observer
./install.sh        # cargo install --path . --locked → ~/.cargo/bin/episode
```

## Quickstart

List the single-session detectors:

```sh
$ episode detectors
retry-with-tweak
revert
user-redirect
```

Inspect what a transcript would produce, without writing anything:

```sh
$ episode observe --dry-run session.jsonl
[
  {
    "detector": "user-redirect",
    "subject": "project:my-repo",
    "body": "...",
    "evidence": [{ "session": "...", "turn": 12, "excerpt": "..." }]
  }
]
```

Without `--dry-run`, `observe` emits nothing on stdout — writing to recall is a downstream concern. `--max-memories N` caps the candidates per session (default 5). Malformed JSONL lines are skipped with a note on stderr; the stream continues.

## Detectors

**Single session** — `episode observe`:

| Detector | Fires when |
|---|---|
| `user-redirect` | A user message opens with a redirect word (`no`, `stop`, `don't`, `actually`, `wait`) within two turns of an assistant tool-use. |
| `revert` | An edit returns a file to its pre-change state within five turns. |
| `retry-with-tweak` | A failing `Bash` command is followed within three turns by a similar one (token Jaccard ≥ 0.5) that succeeds. |

**Across sessions** — `episode observe-cross --since 2h --transcripts-dir <dir>`:
correlates patterns across the transcripts modified in a window — corrective (one session errors on a path, another fixes it within 300s), redundant (two sessions editing the same file), and rescue (one session stalls, another picks up its slug and paths). It emits `episodic-cross-session` candidates and, with `--write-proposals`, writes recall proposal files (default `~/.claude/recall/proposals/cross-session/`). `--dry-run` prints to stdout and writes nothing.

## Where it fits

Part of the recall memory stack. `episode` is the producer; recall is the consumer. Intended to run from a Stop hook after a session ends, feeding candidates into the recall write path — that wiring lives downstream of this repo.

## Status

The detectors and CLI are complete and covered by integration tests (one per acceptance criterion under `tests/`). `observe` is single-session; `observe-cross` is the cross-session correlator added in v0.2.0. The Stop-hook integration and the recall-write step are not in this repo — `episode` ends at producing well-typed candidates.

## Provenance

Built via the [`autobuilder`](https://github.com/j0yen/autobuilder) pipeline (PRD → intent-card → scaffold → iterate-and-prove). Originally a subdir of the [`wintermute`](https://github.com/j0yen/wintermute) monorepo; this is a fresh-init standalone snapshot.

## License

Apache-2.0 OR MIT, at your option ([LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT)).
