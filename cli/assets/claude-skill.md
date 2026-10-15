---
name: madelog
description: Use when working in a repository that has a .madelog/ directory, or when the user mentions madelog, their project page, "write down a decision", publishing their process, the hash chain, AI-session attribution, or exporting a report. Covers recording decisions with madelog note, checking what is recorded, publishing, and keeping the page honest.
---

# madelog

madelog runs beside git and records how a project came together: commits, the AI sessions behind them, and one-sentence decisions. Every record is hash-chained in `.madelog/log.db`. `madelog publish` renders one page a recruiter judges in about six seconds.

Check first: `madelog --version` and `madelog status`. If the repo has no `.madelog/`, ask before running `madelog init`.

## Automatic, like git

Once a repo has `.madelog/`, madelog runs on its own. Do not ask before recording.

- **Commits:** the post-commit hook records each commit, attaches the AI sessions behind it, analyses patterns, summarises the week and re-renders the local page. Nothing to run.
- **Decisions:** whenever a real decision is made in the session (a library chosen, an approach rejected, an architecture changed, a scope cut), record it right away, without asking, as one plain sentence in the user's voice:
  ```sh
  madelog note "Server in Go, not Rust: three endpoints, one binary."
  ```
  Start with the verb and leave the person's name out ("Chose …", "Moved …", "Dropped …"): the note is already theirs, and it is printed as-is on the recruiter sheet. Keep it under 140 characters. Then mention it in one line ("Recorded: …"). Record a decision once, when it is settled, not every option discussed.
  Add `--private` automatically for anything about clients, money, people or credentials. Use `--at <sha>` when the decision belongs to an earlier commit.
- **Not automatic:** `publish`, `unpublish`, `pro` and `init`. Like `git push`, these leave the machine or change setup, so they wait for the user.

## Commands you will use

| Need | Command |
|---|---|
| What is recorded, hooks, server, page | `madelog status` |
| Which AI sessions a commit would get | `madelog capture` (dry run), `madelog capture --record` |
| Something looks broken | `madelog doctor`, then `madelog doctor --fix` |
| Verify the chain | `madelog verify` |
| Raw log as JSON | `madelog export [file]` |
| The page as stamped JSON with status | `madelog export --report [file]` |
| Set the project status | `madelog config set project.status active\|paused\|delivered\|archived` (empty = derived) |
| Hide one AI session from the page | `madelog hide <event_id>` |
| Correct a recorded decision | `madelog note --amends <decision_id> "corrected sentence"` (appends; the page shows the newest) |
| Choose which decisions lead the sheet | `madelog pin <decision_id>`, `madelog pin --remove <decision_id>` |
| Find event ids | `madelog log --json` (ids are time-based, so give the full id) |
| Markdown for applications | `madelog export --report --md` |
| README badge (published repos) | `![madelog](https://api.madelog.app/p/<slug>/badge.svg)` linking to the page |
| Render locally | `madelog publish --local`, then `madelog open` |
| Put it online | `madelog publish --yes` (prints the link; Pro also prints a PDF link) |
| Take it down | `madelog unpublish` |
| Record commits from before madelog | `madelog backfill` |

## Rules

- **Never publish, unpublish, or run `madelog pro` without the user saying so.** Publishing sends the page to a public server. Recording notes needs no permission.
- **Never invent numbers.** Everything on the page comes from the log. If a figure looks wrong, find the cause in the events (`madelog log --json`) and fix the code or the record. Do not edit the page HTML by hand.
- **Never edit `.madelog/log.db`.** The chain is append-only; a change breaks `madelog verify` and the server's root no longer matches. To correct something, append: `note --amends`, a new `init --what …` intent, `pin`, or `hide`. Fix a typo in a decision with `--amends` when the user asks; never on your own.
- **Never commit `.madelog/`.** It is excluded in `.git/info/exclude`.
- Decisions are one sentence, specific, and say what was chosen over what. No marketing words.

## Reading the page honestly

- Attribution covers only commits that carry a recorded AI session. Other lines are "not attributed", never "written by hand".
- Commits removed by a rebase or history purge stay in the chain and are left out of every figure.
- Pattern timings use each commit's own date, so backfilled history is timed correctly.

## Troubleshooting

- `madelog: run madelog init here` on commit: global hooks are on and this repo is not set up. Ask the user.
- No AI sessions attached: run `madelog capture` on that commit; it explains which sessions it saw and why each was or was not matched.
- Server problems: the API is `https://api.madelog.app`; `madelog doctor` checks it.
