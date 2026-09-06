# Hermes YOLO and Remembered Workspaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add verified Hermes YOLO launches and a local remembered-workspace picker to the OAV new-session composer.

**Architecture:** Keep provider behavior in the existing native SQLite adapter and make Hermes the sole newly verified YOLO mapping. Add one local `workspaces` state module, modeled on `hidden.rs` security guarantees, and carry immutable workspace and `yolo_armed` values on each launch action through the App, terminal dispatcher, and ControlHub; neither setting mutates OAV's own process CWD or becomes dashboard-wide state.

**Tech Stack:** Rust 2021/MSRV 1.75, Ratatui, Crossterm, Serde JSON, existing OAV private-state and native-PTY infrastructure.

---

## File structure

| File | Responsibility |
| --- | --- |
| `src/workspaces.rs` | Private persisted workspace records; validation, newest-first ordering, lock/atomic upsert, and tests. |
| `src/lib.rs` | Exposes `workspaces`. |
| `src/app.rs` | Tracks active launch workspace and workspace-picker state; emits exact workspace paths in actions. |
| `src/control.rs` | Builds each `LaunchRequest` from the workspace attached to a single action. |
| `src/terminal.rs` | Validates picker/path selections via the store and records a workspace only after a successful foreground or asynchronous launch. |
| `src/main.rs` | Loads the store, resolves startup precedence, and passes it into the dashboard. |
| `src/adapters/native_owned.rs` | Stores the YOLO launch marker for OAV-owned native sessions. |
| `src/adapters/session_migrate_native.rs` | Adds Hermes-only YOLO capability/argv and preserves the foreground SQLite prompt sequence. |
| `src/ui.rs` | Renders the workspace in the composer and the workspace picker/help. |
| `README.md`, `docs/cli.md`, `docs/control-model.md`, `docs/exploration/shared-sqlite-harnesses.md`, `CHANGELOG.md` | Document the exact Hermes mapping and workspace persistence contract. |

### Task 1: Establish the fork execution contract

**Files:**
- Create: `.ai/orchestration-adapter.md`

Upstream has no `.ai/orchestration-adapter.md`. Before dispatching implementation workers, add a fork-local adapter with `default_branch: main`, Rust 1.75 setup/validation commands, `/home/radan/Projects/OpenAgentView/open-agent-view/.worktrees/pr<NUMBER>` worktree convention, host parallelism of one heavy Cargo job, and `none` for GitHub-specific closure fields until a feature PR exists. The final upstream PR must exclude the adapter if upstream does not want orchestration metadata.

- [ ] **Step 1: Record the preflight adapter and verify it is isolated from the feature diff**

Use the fork's actual local commands:

```bash
git -C /home/radan/Projects/OpenAgentView/open-agent-view status --short --branch
cargo +1.75.0 test --locked
cargo +1.75.0 build --release --locked
cargo test --locked
cargo build --release --locked
```

Expected: the branch is a named feature/worktree branch, all four commands exit 0 before source changes, and no direct commit is made to `main`.

- [ ] **Step 2: Commit the fork-only execution metadata separately**

```bash
git add .ai/orchestration-adapter.md
git commit -m "chore: add OAV orchestration adapter"
```

Expected: this commit stays separate from product commits so it can be omitted from the future upstream PR.

### Task 2: Add a secure, minimal remembered-workspace store

**Files:**
- Create: `src/workspaces.rs`
- Modify: `src/lib.rs:3-20`
- Test: inline `src/workspaces.rs` tests

- [ ] **Step 1: Write failing state and safety tests**

Add tests covering exact durable behavior:

```rust
#[test]
fn successful_workspace_upsert_is_persistent_newest_first_and_deduplicated() {
    let root = private_tempdir();
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let store = Workspaces::load(root.path().join("workspaces.json")).unwrap();

    store.record_successful_launch(&first).unwrap();
    store.record_successful_launch(&second).unwrap();
    store.record_successful_launch(&first).unwrap();

    assert_eq!(store.list(), vec![first.clone(), second]);
    assert_eq!(Workspaces::load(store.path()).unwrap().list()[0], first);
}

#[test]
fn relative_missing_file_symlinked_and_insecure_state_are_refused() {
    let root = private_tempdir();
    let store = Workspaces::load(root.path().join("workspaces.json")).unwrap();
    assert!(store.validate_selection(Path::new("relative")).is_err());
    assert!(store.validate_selection(&root.path().join("missing")).is_err());
    let file = root.path().join("file");
    fs::write(&file, "not a directory").unwrap();
    assert!(store.validate_selection(&file).is_err());
    // On Unix, also assert a symlinked registry and mode-0644 registry are refused.
}
```

- [ ] **Step 2: Run the new tests to prove they fail**

Run:

```bash
cargo test --locked workspaces::tests -- --nocapture
```

Expected: compile failure because `workspaces` and `Workspaces` do not exist.

- [ ] **Step 3: Implement `src/workspaces.rs` without a generic registry abstraction**

Define these exact public operations:

```rust
pub struct WorkspaceRecord {
    pub path: PathBuf,
    pub last_used_at_ms: u64,
}

pub struct Workspaces { /* private path + Arc<Mutex<BTreeMap<PathBuf, WorkspaceRecord>>> */ }

impl Workspaces {
    pub fn load_default() -> Result<Self>;
    pub fn load(path: impl Into<PathBuf>) -> Result<Self>;
    pub fn path(&self) -> &Path;
    pub fn list(&self) -> Vec<PathBuf>;
    pub fn validate_selection(&self, path: &Path) -> Result<PathBuf>;
    pub fn record_successful_launch(&self, path: &Path) -> Result<PathBuf>;
}
```

Use `hidden.rs` as the concrete safety model, not a new shared framework: state root is `$XDG_STATE_HOME/open-agent-view/workspaces.json` or `~/.local/state/open-agent-view/workspaces.json`; directory mode is 0700, state/lock modes are 0600 on Unix, symlinks and wrong owners/modes fail closed, mutations take a `workspaces.lock`, reload under lock, write a versioned JSON document to a same-directory unique temporary file, `sync_all`, and call `fs_util::replace_file`.

`validate_selection` must reject non-absolute paths, non-existent entries, and entries that are not directories, then return a canonical absolute path. `record_successful_launch` must call that validator, upsert by canonical path, use the current millisecond timestamp, sort `list()` descending by timestamp then path for deterministic ties, and never create a record before a successful controller result.

Add `pub mod workspaces;` beside `pub mod hidden;` in `src/lib.rs`.

- [ ] **Step 4: Run focused store tests**

Run:

```bash
cargo test --locked workspaces::tests
cargo fmt --all -- --check
```

Expected: both commands exit 0.

- [ ] **Step 5: Commit the store**

```bash
git add src/lib.rs src/workspaces.rs
git commit -m "feat: persist successful launch workspaces"
```

### Task 3: Make selected workspace and YOLO intent part of one launch contract

**Files:**
- Modify: `src/app.rs:27-177, 1250-1357`
- Modify: `src/control.rs:439-515`
- Modify: `src/terminal.rs:39-84, 480-572, 1184-1306, 1415-1445`
- Modify: `src/main.rs:553-596, 900-940`
- Test: inline `src/app.rs`, `src/control.rs`, and `src/terminal.rs` tests

- [ ] **Step 1: Write failing App and ControlHub tests for per-launch CWD**

Add a pure App transition test and a ControlHub request test that assert the path travels on one exact action:

```rust
#[test]
fn new_task_action_carries_selected_workspace_and_yolo_intent() {
    let workspace = PathBuf::from("/absolute/project");
    let mut app = app_with_launch_workspace(workspace.clone());
    app.open_composer();
    app.input = "fix the parser".into();
    assert_eq!(app.activate(), AppAction::Launch {
        provider: Provider::Claude,
        model: None,
        prompt: "fix the parser".into(),
        cwd: workspace,
        yolo: true,
    });
}

#[test]
fn launch_with_options_uses_action_workspace_and_yolo_not_startup_defaults() {
    let outcome = hub.launch_with_options(
        Provider::Hermes,
        None,
        "task".into(),
        PathBuf::from("/chosen/workspace"),
        true,
    );
    assert_request_cwd(outcome, "/chosen/workspace");
}
```

- [ ] **Step 2: Run the focused tests to prove the API gap**

Run:

```bash
cargo test --locked new_task_action_carries_selected_workspace_and_yolo_intent
cargo test --locked launch_with_options_uses_action_workspace_and_yolo_not_startup_defaults
```

Expected: compilation fails because `AppAction::Launch` has neither `cwd` nor `yolo`, and `launch_with_options` does not exist.

- [ ] **Step 3: Thread `cwd` and `yolo` through App, ControlHub, and both terminal launch paths**

Apply these signatures and retain existing validation/YOLO branches unchanged:

```rust
// src/app.rs
AppAction::Launch {
    provider,
    model,
    prompt,
    cwd: self.launch_cwd.clone(),
    yolo: self.yolo_armed,
}

// src/control.rs
pub fn launch_with_options(
    &self,
    provider: Provider,
    model: Option<String>,
    prompt: String,
    cwd: PathBuf,
    yolo: bool,
) -> Result<ControlOutcome> {
    let request = LaunchRequest { provider, model: validate_model(model)?, prompt, cwd };
    if yolo && !controller.supports_yolo() {
        bail!("{} does not expose a verified permission-bypass mode", provider.label());
    }
    if yolo {
        controller.launch_yolo(&request)
    } else {
        controller.launch(&request)
    }
}
```

Refactor `launch_with` and `launch_foreground_with` into safe wrappers so existing callers/tests remain valid. Extend `DashboardControl`, its `ControlHub` implementation, `LaunchJob`, `LaunchWorkerResult`, `schedule_launch`, and `dispatch_foreground_launch` to carry both `cwd` and `yolo`. Remove the dashboard-wide `ControlHubConfig.yolo` policy: the controller sees only the exact launch option. Do not read a mutable global cwd or change OAV's process directory.

At startup, load `Workspaces`, then resolve `cli.launch_cwd` or `std::env::current_dir()`; do not read the workspace store to choose a startup path. Validate the selected workspace before constructing `ControlHub`. Pass a clone of `Workspaces` and the initial `cli.yolo` armed value into `run_dashboard`/`App`; `--yolo` must not remain in `ControlHub` after startup.

- [ ] **Step 4: Record only successful launches**

For foreground launches, call `workspaces.record_successful_launch(&cwd)` and `app.consume_yolo_if(yolo)` only after `launch_foreground_session` returns `Ok`. For asynchronous launches, include `cwd` and `yolo` in `LaunchWorkerResult` and do both only in the `Ok(outcome)` receive branch. Failed/refused launches retain `yolo_armed`. If workspace persistence fails after a provider launch, leave the provider success intact, keep the chosen in-memory CWD, and show a notice that the workspace could not be remembered.

After a successful store update, call an App method that replaces its picker list with `workspaces.list()`; no failed launch may mutate that list.

- [ ] **Step 5: Run the focused launch-contract tests**

Run:

```bash
cargo test --locked app::tests
cargo test --locked control::tests
cargo test --locked terminal::tests
cargo fmt --all -- --check
```

Expected: tests pass; normal launch behavior remains unchanged when the picker is unused.

- [ ] **Step 6: Commit the per-launch workspace plumbing**

```bash
git add src/app.rs src/control.rs src/terminal.rs src/main.rs
git commit -m "feat: launch sessions from selected workspaces"
```

### Task 4: Add the remembered-workspace composer picker

**Files:**
- Modify: `src/app.rs:27-177, 430-535, 603-832, 1250-1357`
- Modify: `src/ui.rs:412-531, 708-751, 883-949`
- Modify: `src/terminal.rs:480-572`
- Test: inline `src/app.rs` and `src/ui.rs` tests; `tests/real_tty.rs` for a disposable picker interaction

- [ ] **Step 1: Write failing workspace-picker behavior tests**

Cover command routing, filtering, selection, and escape without touching the filesystem from `App`:

```rust
#[test]
fn workspace_command_opens_filters_selects_and_escapes_without_losing_draft() {
    let alpha = PathBuf::from("/projects/alpha");
    let beta = PathBuf::from("/projects/beta");
    let mut app = app_with_workspaces(alpha.clone(), vec![alpha.clone(), beta]);
    app.open_composer();
    app.input = "/workspace".into();
    assert_eq!(app.activate(), AppAction::None);
    assert_eq!(app.overlay, Overlay::WorkspacePicker);

    app.push_input('b');
    assert_eq!(app.activate(), AppAction::SelectWorkspace { cwd: PathBuf::from("/projects/beta") });
    app.set_launch_workspace(PathBuf::from("/projects/beta"));
    assert_eq!(app.launch_cwd, PathBuf::from("/projects/beta"));

    app.open_workspace_picker();
    app.escape();
    assert_eq!(app.launch_cwd, PathBuf::from("/projects/beta"));
}
```

Also assert `/workspace /new/path` emits `SelectWorkspace`, malformed/relative/missing paths are refused by the terminal/store and leave `app.launch_cwd` unchanged, and rendered composer/help text contains the active workspace plus `/workspace` guidance.

- [ ] **Step 2: Run picker tests to prove they fail**

Run:

```bash
cargo test --locked workspace_command_opens_filters_selects_and_escapes_without_losing_draft
cargo test --locked ui::tests
```

Expected: compile failure because `WorkspacePicker`, `SelectWorkspace`, and workspace state are absent.

- [ ] **Step 3: Implement one keyboard interaction model using existing picker conventions**

Add `Overlay::WorkspacePicker`, `workspace_choices: Vec<PathBuf>`, `workspace_filter: String`, and `workspace_selection: usize` to `App`. Implement:

```rust
pub fn open_workspace_picker(&mut self) -> AppAction;
pub fn replace_workspace_choices(&mut self, choices: Vec<PathBuf>);
pub fn set_launch_workspace(&mut self, cwd: PathBuf);
fn workspace_choices(&self) -> Vec<&PathBuf>;
fn select_workspace_command(&mut self, argument: &str) -> AppAction;
```

`/workspace` opens the picker; `/workspace /absolute/path` emits `AppAction::SelectWorkspace { cwd }`; the picker uses its own filter field, arrows/Tab move through newest-first choices, Enter emits the same action, and Esc returns to the composer with the task draft and active path unchanged. Empty remembered state shows a direct instruction to use `/workspace /absolute/path`; it never opens a filesystem browser.

In the terminal event loop, handle `SelectWorkspace` before controller dispatch: call `workspaces.validate_selection`, then `app.set_launch_workspace` on success; on error set `workspace unavailable: …` and preserve the old choice. Update `ui.rs` to include `workspace <path>` in the new-task border title, render a picker matching the harness/model visual pattern with full absolute paths as its only row label, and expose `/workspace` in contextual help and footer. Do not bind Ctrl+W: it is already the portable delete-previous-word editing key.

- [ ] **Step 4: Add a disposable real-PTY regression**

Extend `tests/real_tty.rs` with a fixture-backed dashboard test that starts with two private test workspaces, opens `/workspace`, filters/selects the second path, confirms the composer title changes, launches a fixture provider, backgrounds/returns, restarts OAV from the first path, and confirms the first path is selected while the second remains in the picker. The test must use temporary state and fixture executables only; it must not access personal sessions or credentials.

- [ ] **Step 5: Run picker checks**

Run:

```bash
cargo test --locked workspace_command_opens_filters_selects_and_escapes_without_losing_draft
cargo test --locked ui::tests
cargo test --locked --test real_tty workspace -- --test-threads=1
cargo fmt --all -- --check
```

Expected: all focused tests pass; the real-PTY test proves terminal restoration and persisted selection without authentication.

- [ ] **Step 6: Commit the picker**

```bash
git add src/app.rs src/ui.rs src/terminal.rs tests/real_tty.rs
git commit -m "feat: choose remembered launch workspaces"
```

### Task 5: Add and consume the one-session YOLO composer setting

**Files:**
- Modify: `src/app.rs:27-177, 430-535, 1250-1357`
- Modify: `src/terminal.rs:450-572`
- Modify: `src/ui.rs:412-531, 708-751, 883-949`
- Modify: `src/main.rs:553-596`
- Modify: `src/native_session.rs:131-207`
- Modify: `src/adapters/native_owned.rs:19-96`
- Test: inline `src/app.rs`, `src/terminal.rs`, and `src/ui.rs` tests

- [ ] **Step 1: Write failing composer-state tests**

```rust
#[test]
fn yolo_confirmation_arms_only_the_next_successful_launch() {
    let mut app = app_with_launch_workspace(PathBuf::from("/work"));
    app.open_composer();
    app.input = "/yolo".into();
    assert_eq!(app.activate(), AppAction::None);
    assert_eq!(app.overlay, Overlay::Confirm(ConfirmTarget::EnableYolo));

    app.confirm_yolo(true);
    assert!(app.yolo_armed);
    app.select_launch_provider("hermes");
    app.select_launch_model("openai-codex/gpt-5.4-mini");
    assert!(app.yolo_armed);

    app.consume_yolo_if(true);
    assert!(!app.yolo_armed);
}

#[test]
fn yolo_disarms_without_confirmation_and_a_failed_launch_does_not_consume_it() {
    let mut app = app_with_yolo_armed();
    app.submit_new_session("task".into());
    assert!(app.yolo_armed);
    app.submit_new_session("/yolo".into());
    assert!(!app.yolo_armed);
}
```

- [ ] **Step 2: Run the tests to prove the composer capability is absent**

Run:

```bash
cargo test --locked yolo_confirmation_arms_only_the_next_successful_launch
cargo test --locked yolo_disarms_without_confirmation_and_a_failed_launch_does_not_consume_it
```

Expected: compilation fails because `EnableYolo`, `yolo_armed`, confirmation, and consumption behavior do not exist.

- [ ] **Step 3: Implement the explicit confirm/disarm state machine**

Add `ConfirmTarget::EnableYolo` and `yolo_armed: bool` to `App`. `/yolo` when false opens the ordinary confirm overlay with exact text `Enable YOLO for the next launched session? y/N`; `y`/Enter confirms, `n`/Esc cancels, and `/yolo` when true clears the bit directly. `--yolo` initializes that same bit during App construction. `/harness`, `/model`, `/workspace`, and draft edits do not alter it.

Render `⚠ YOLO · next session only` in the composer title only while armed, alongside the current harness/model/workspace. Add `/yolo` to contextual help. Do not add a dashboard-wide warning or a persistent preference.

- [ ] **Step 4: Preserve visibility on the created native session**

Extend `OwnedNativeSession` in `src/adapters/native_owned.rs` with a serde-defaulted `yolo: bool`, pass it into `NativeOwnership::record`, and expose it when building an OAV-owned native row. Add a small `run_with_screen_steps_yolo` helper in `src/native_session.rs` that uses the existing warning plumbing and is selected only for an armed Hermes launch. The warning must survive OAV's background/re-entry route for the retained frontend; safe session records and old ownership JSON default to `false`.

- [ ] **Step 5: Consume only after the exact launch succeeds**

In the foreground and asynchronous terminal paths, call `app.consume_yolo_if(yolo)` only after the controller returns `Ok(ControlOutcome)`. A missing controller, unsupported harness, validation failure, provider exit, or discovery/correlation failure leaves the composer armed so the user can correct and retry. Add terminal tests that exercise both success and refusal.

- [ ] **Step 6: Run focused checks and commit**

```bash
cargo test --locked yolo_
cargo test --locked app::tests
cargo test --locked terminal::tests
cargo fmt --all -- --check
git add src/app.rs src/terminal.rs src/ui.rs src/main.rs src/native_session.rs src/adapters/native_owned.rs
git commit -m "feat: arm YOLO for one launch"
```

Expected: YOLO is safe-by-default after every successful launch, without losing a failed launch's armed state.

### Task 6: Enable Hermes-only verified YOLO launch

**Files:**
- Modify: `src/adapters/session_migrate_native.rs:197-301, 321-326, 723-800, 1542-1575`
- Modify: `src/control.rs:318-328`
- Test: inline `src/adapters/session_migrate_native.rs`; `tests/real_tty.rs` or the existing disposable SQLite-native PTY probe

- [ ] **Step 1: Write failing safe/YOLO command and capability tests**

Split the existing mixed native-command assertion so Hermes has an exact positive YOLO contract:

```rust
#[test]
fn hermes_yolo_armed_launch_uses_global_cli_flag_and_safe_launch_omits_it() {
    let workspace = tempfile::tempdir().unwrap();
    let request = LaunchRequest {
        provider: Provider::Hermes,
        model: Some("openai-codex/gpt-5.4-mini".into()),
        prompt: "fix tests".into(),
        cwd: workspace.path().to_owned(),
    };
    let safe = launch_command(&Provider::Hermes, "hermes", &request, false).unwrap();
    assert_eq!(args(&safe), ["chat", "--cli", "--model", "openai-codex/gpt-5.4-mini"]);
    let yolo = launch_command(&Provider::Hermes, "hermes", &request, true).unwrap();
    assert_eq!(args(&yolo), ["--yolo", "chat", "--cli", "--model", "openai-codex/gpt-5.4-mini"]);
}

#[test]
fn yolo_support_includes_hermes_but_not_mastracode_or_devin() {
    let root = tempfile::tempdir().unwrap();
    for (provider, expected) in [
        (Provider::Hermes, true),
        (Provider::MastraCode, false),
        (Provider::Devin, false),
    ] {
        let ownership = SessionMigrateNativeOwnership::load(
            provider.clone(),
            root.path().join(format!("{provider}-owned.json")),
        ).unwrap();
        let controller = SessionMigrateNativeController::host(
            provider,
            "native-cli",
            root.path().join("state"),
            ownership,
        ).unwrap();
        assert_eq!(controller.supports_yolo(), expected);
    }
}
```

- [ ] **Step 2: Run these tests to prove the current fail-closed behavior**

Run:

```bash
cargo test --locked adapters::session_migrate_native::tests::hermes_yolo -- --exact
```

Expected: failure because Hermes currently rejects YOLO.

- [ ] **Step 3: Implement only the Hermes exception**

In `launch_command`, replace the Hermes refusal with a global flag inserted before the existing command:

```rust
Provider::Hermes => {
    if yolo {
        command.arg("--yolo");
    }
    command.args(["chat", "--cli"]);
    if let Some(model) = &request.model {
        command.args(["--model", model]);
    }
}
```

Change `supports_yolo` to include `Provider::Hermes` and change the SQLite foreground launch guard from blanket `if yolo { bail!(...) }` to permit `Provider::Hermes` only. An armed Hermes launch must use `run_with_screen_steps_yolo` so its ready marker, bracketed-paste prompt injection, database correlation, ownership write, warning, and background/resume semantics remain exactly the same. Preserve explicit refusal for MastraCode and Devin; do not add a generic `sqlite supports yolo` rule.

- [ ] **Step 4: Add a disposable foreground regression and run focused checks**

Extend the existing Hermes native fixture/probe to record argv and run an OAV `--yolo` pre-armed foreground launch. Assert the composer starts armed, the fixture receives `--yolo` for that first launch only, the retained native warning remains visible, a prompt is injected only after Hermes readiness, the owned session is discovered in the selected workspace, and returning/backgrounding preserves the exact session. Assert the next composer task is unarmed.

```bash
cargo test --locked adapters::session_migrate_native::tests
cargo test --locked --test real_tty hermes -- --test-threads=1
```

Expected: both pass with no real account or credential access.

- [ ] **Step 5: Commit the Hermes mapping**

```bash
git add src/adapters/session_migrate_native.rs src/control.rs tests/real_tty.rs
git commit -m "feat: support Hermes YOLO launches"
```

### Task 7: Document the exact behavior and run the release-quality gate

**Files:**
- Modify: `README.md:85-92, 164-181`
- Modify: `docs/cli.md:33-110, 151-188`
- Modify: `docs/control-model.md:68-94`
- Modify: `docs/exploration/shared-sqlite-harnesses.md:21-50`
- Modify: `CHANGELOG.md`
- Test: `tests/readme_metadata.rs` when its expected harness/feature text changes

- [ ] **Step 1: Write documentation assertions or update existing metadata assertions first**

Add/update deterministic assertions so public docs contain all of:

```text
Hermes Agent | --yolo
/yolo
next session only
/workspace
remembered workspaces
--launch-cwd
```

Assert that docs still name MastraCode as unsupported for YOLO and describe both `--yolo` and `/yolo` as an explicit opt-in for one new session only.

- [ ] **Step 2: Run doc/metadata tests to prove required text is absent or stale**

Run:

```bash
cargo test --locked --test readme_metadata
```

Expected: the test identifies the missing Hermes mapping and workspace command before documentation edits.

- [ ] **Step 3: Update operator documentation without widening claims**

Document `hermes --yolo chat --cli` as the mapping OAV uses; say it bypasses Hermes dangerous-command approvals and is disabled by default. Document that `--yolo` pre-arms the next session and `/yolo` asks `Enable YOLO for the next launched session? y/N`; confirmation survives launch-option changes and failed launches, then is consumed by success. Document `/workspace`, `/workspace /absolute/path`, newest-first persistence after successful launches only, current-directory startup, and one-run `--launch-cwd` precedence. Do not claim a filesystem browser, labels, sync, automatic scanning, or authenticated provider validation.

- [ ] **Step 4: Run focused documentation and full code gates serially**

Run one heavy command at a time:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo +1.75.0 test --locked
cargo +1.75.0 build --release --locked
cargo test --locked
cargo build --release --locked
cargo test --locked --test readme_metadata
```

Expected: each exits 0. If a real-PTY test is credential-gated, report it as not run; do not replace it with a claim of authenticated proof.

- [ ] **Step 5: Commit documentation and inspect the feature branch**

```bash
git add README.md docs/cli.md docs/control-model.md docs/exploration/shared-sqlite-harnesses.md CHANGELOG.md tests/readme_metadata.rs
git commit -m "docs: describe Hermes YOLO and workspaces"
git status --short --branch
git log --oneline origin/main..HEAD
```

Expected: the feature branch is clean, contains focused commits, and has no direct `main` commit.

## Review and PR gate

- [ ] Verify all feature commits are pushed to a branch based on the current `origin/main`.
- [ ] Run an independent cross-family review on the exact pushed tip; its coverage matrix must include safe vs YOLO Hermes argv, unsupported-provider refusal, workspace state-file security, failure-no-save behavior, picker escape/filter ordering, startup precedence, and both background/foreground launch paths.
- [ ] Resolve every blocking review finding, rerun the affected focused tests, then rerun the complete Task 7 gate once.
- [ ] Open a PR from the fork to `xhluca/open-agent-view:main` with `Refs #3` only if upstream issue #3 remains the relevant tracking issue; otherwise describe both features without claiming it closes an unrelated issue. Do not merge without Radan's explicit per-PR authorization.
