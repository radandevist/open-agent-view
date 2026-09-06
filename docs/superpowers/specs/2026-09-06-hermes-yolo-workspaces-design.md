# Hermes YOLO and remembered workspaces

## Goal

Make daily OAV launches work for Hermes Agent with its verified explicit YOLO mode and let the composer choose from a fixed, local list of previously successful workspaces.

## Scope

1. Hermes YOLO
   - `open-agent-view --yolo` pre-arms YOLO for exactly the next new session; it does not make the dashboard globally YOLO.
   - `/yolo` is the equivalent composer command. When currently off, it opens `Enable YOLO for the next launched session? y/N`; after confirmation it is visible beside the selected harness and model. Calling `/yolo` again disarms it without another confirmation.
   - The armed setting survives edits to the draft, harness, model, and workspace. A failed/refused launch keeps it so the task can be retried; a successful launch consumes it and restores safe-by-default behavior for the next task.
   - Hermes is added only after mapping the exact verified native invocation:
     `hermes --yolo chat --cli [--model ID]`.
   - Normal Hermes launches remain unchanged and do not receive `--yolo`.
   - Arming is harness-agnostic so `/yolo`, `/harness`, and `/model` can be used in any order. Launching an unsupported harness remains a fail-closed refusal before a provider process starts.
   - The created YOLO session remains visibly marked in its retained native frontend and OAV ownership metadata after the composer setting is consumed.
   - The foreground Hermes TUI launch path must retain its existing prompt injection, ownership record, workspace correlation, and resume behavior.

2. Remembered workspaces
   - The selected workspace is visible in the new-task composer and is used for every new launch, independent of harness and model selection.
   - `/workspace` opens a keyboard-filterable picker of remembered workspaces. It supports filter text, arrows/Tab, Enter to select, and Esc to leave the selection unchanged. Every row renders the full absolute path; there are no derived project names or stored labels.
   - `/workspace /absolute/path` selects one existing absolute directory for the current OAV process. It is not immediately saved.
   - After a successful launch, OAV atomically records the workspace and last-used timestamp. Failed/refused launches do not add it.
   - The picker is a fixed remembered list: OAV does not browse the filesystem, crawl projects, synchronize paths, or create labels.
   - On restart, the most recently successful existing workspace is selected. `--launch-cwd PATH` is a one-run override and wins over persisted selection. If the stored path has disappeared or is invalid, skip it and fall back to the process directory.

## Storage and safety

Store only absolute workspace paths and last-used timestamps in OAV's existing private state root (`$XDG_STATE_HOME/open-agent-view`, otherwise `~/.local/state/open-agent-view`). Reuse the project's current-user ownership, restrictive-mode, locking, and atomic-replace conventions. Validate directories before selection and again before launching. Do not store prompts, model IDs, provider credentials, session history, or directory contents.

The initial default process directory and `--launch-cwd` are not persisted until a launch using that directory succeeds. A failed launch cannot pollute the picker.

## Design boundaries

This is intentionally two focused capabilities, not a generic workspace manager:

- no directory browser or recursive scanning;
- no manually curated labels, pinned entries, import/export, or cloud sync;
- no change to the existing `--launch-cwd` CLI contract;
- no YOLO change to existing sessions, discovery, or any unsupported harness.

## Implementation shape

- Extend the native SQLite-harness controller's verified YOLO capability and argv construction for `Provider::Hermes` only.
- Permit the existing foreground SQLite Hermes launch/prompt sequence to run in the explicit YOLO path; preserve the fail-closed guard for every other unverified provider.
- Add a small state component for loading, validating, selecting, and upserting remembered workspaces.
- Carry the selected workspace and one-session YOLO bit through `App`, `AppAction::Launch`, the terminal dispatcher, and `ControlHub` so both choices apply to one exact launch request rather than changing process CWD or dashboard-wide security state.
- Add a workspace picker overlay following the existing harness/model picker interaction and render the active workspace in composer/help text.
- Update the CLI guide, control model, exploration note, README harness behavior, and changelog to state the exact Hermes mapping and workspace contract.

## Acceptance criteria

### Hermes YOLO

- With OAV YOLO disabled, an Hermes launch argv contains `chat --cli` and not `--yolo`.
- `--yolo` and a confirmed `/yolo` each arm the next launch only; the composer shows that state, a failed/refused launch retains it, and a successful launch consumes it.
- An armed Hermes launch argv contains the global `--yolo` before `chat --cli`, preserves a selected `--model`, and starts from the selected workspace.
- An armed session can change harness/model before launch; MastraCode remains unsupported and fails before provider launch if selected.
- The Hermes foreground launch retains its queued prompt, created-session correlation, ownership persistence, background/resume behavior, and a visible YOLO marker in YOLO mode.

### Workspaces

- A valid absolute path selected through `/workspace PATH` is shown in the composer and is sent as the launch request CWD.
- Relative, missing, non-directory, and unsafe paths are rejected without changing selection.
- A successful launch upserts the selected workspace; a failed/refused launch leaves the store unchanged.
- The picker orders valid remembered entries newest first, filters by path, selects deterministically, and leaves state unchanged on Esc.
- Restart chooses the latest valid remembered workspace; an explicit `--launch-cwd` overrides it without changing the persisted default until it succeeds in a launch.
- State writes are user-private, atomically replaced, and reject malformed/untrusted existing state according to OAV's current state-file rules.

## Verification

Run focused unit tests for native Hermes argv/control and workspace state/app transitions first. Then run the repository's documented gates:

```console
cargo +1.75.0 test --locked
cargo +1.75.0 build --release --locked
cargo test --locked
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
```

For the TUI picker and the Hermes native-control path, add deterministic real-PTY coverage using disposable state and fixture executables. Do not claim authenticated provider validation unless it is separately executed with a dedicated disposable identity.

## Deferred work

A filesystem directory browser, manual labels/pins, sharing/sync, and YOLO mappings for other newly supported harnesses are separate features.
