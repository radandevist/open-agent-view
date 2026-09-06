#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use open_agent_view::adapters::{
    AntigravityController, AntigravityOwnership, DiscoveryRequest, MistralVibeController,
    MistralVibeOwnership, MistralVibeSource, QwenController, QwenOwnership, QwenSource,
    SessionMigrateNativeController, SessionMigrateNativeOwnership, SessionSource,
};
use open_agent_view::control::{LaunchRequest, ProviderController};
use open_agent_view::domain::{
    AgentSession, Provider, Runtime, SessionKind, SessionSnapshot, SessionState,
};

const PTY_CHILD: &str = "OAV_MISTRAL_QWEN_PTY_CHILD";

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[test]
fn mistral_controller_background_reattach_interrupt_and_exact_resume_use_real_ptys() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("mistral") {
        run_mistral_pty_child();
        return;
    }
    run_pty_outer(
        "mistral",
        "mistral_controller_background_reattach_interrupt_and_exact_resume_use_real_ptys",
        "VIBE",
        b"VIBE_NATIVE_READY",
    );
}

#[test]
fn qwen_controller_background_reattach_interrupt_and_exact_resume_use_real_ptys() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("qwen") {
        run_qwen_pty_child();
        return;
    }
    run_pty_outer(
        "qwen",
        "qwen_controller_background_reattach_interrupt_and_exact_resume_use_real_ptys",
        "QWEN",
        b"QWEN_NATIVE_READY",
    );
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn qwen_public_controller_launch_discover_open_and_refuse_unowned_interrupt() {
    let directory = private_tempdir();
    let qwen = directory.path().join("qwen");
    executable(
        &qwen,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "${1-} ${2-}" = 'sessions list' ]; then
  [ ! -f "$root/history.jsonl" ] || cat "$root/history.jsonl"
  exit 0
fi
if [ "${1-} ${2-}" = 'sessions ps' ]; then
  exit 0
fi
if [ "${1-}" = '--resume' ]; then
  printf 'resume %s\n' "$2" >> "$root/invocations.log"
  exit 0
fi
session=''
model=''
prompt=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id) session=$2; shift 2 ;;
    --model) model=$2; shift 2 ;;
    --prompt-interactive) prompt=$2; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$session" ] && [ -n "$prompt" ] || exit 64
now=$(($(date +%s) * 1000))
cwd=$(pwd)
printf '{"sessionId":"%s","startTime":"2026-08-25T00:00:00Z","mtime":%s,"prompt":"owned task","customTitle":"Qwen owned","cwd":"%s"}\n' "$session" "$now" "$cwd" > "$root/history.jsonl"
printf 'launch %s %s %s\n' "$session" "$model" "$prompt" >> "$root/invocations.log"
"##,
    );
    let ownership = QwenOwnership::load(directory.path().join("qwen-owned.json")).unwrap();
    let controller = QwenController::host(qwen.display().to_string(), ownership.clone());
    let request = LaunchRequest {
        provider: Provider::QwenCode,
        model: Some("qwen3-coder-plus".into()),
        prompt: "owned task".into(),
        cwd: directory.path().to_owned(),
    };

    let launched = controller.launch_foreground(&request).unwrap();
    let id = launched.provider_session_hint.unwrap();
    let source = QwenSource::host(qwen.display().to_string(), ownership);
    let sessions = source.discover(&DiscoveryRequest::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].provider_session_id, id);
    assert_eq!(sessions[0].provider, Provider::QwenCode);
    assert_eq!(sessions[0].runtime, Runtime::Host);
    controller.open(&sessions[0]).unwrap();
    let invocations = fs::read_to_string(directory.path().join("invocations.log")).unwrap();
    assert!(invocations.contains("qwen3-coder-plus owned task"));
    assert!(invocations.contains(&format!("resume {id}")));

    let mut external = sessions[0].clone();
    external.provider_session_id = "external".into();
    external.id = "qwen:host:external".into();
    assert!(controller.open(&external).is_err());
    assert!(controller.interrupt(&external).is_err());
}

#[test]
fn qwen_restarted_yolo_open_keeps_resume_argv_and_security_mode() {
    let directory = private_tempdir();
    let qwen = directory.path().join("qwen");
    executable(
        &qwen,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' "$*" >> "$root/invocations.log"
exit 0
"##,
    );
    let id = "11111111-2222-4333-8444-555555555555";
    let state = directory.path().join("qwen-owned.json");
    fs::write(
        &state,
        format!(
            r#"[{{"sessionId":"{id}","cwd":"{}","createdAtMs":1,"name":"YOLO task","yolo":true}}]"#,
            directory.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = QwenOwnership::load(state).unwrap();
    let controller = QwenController::host(qwen.display().to_string(), ownership);
    let session = AgentSession {
        id: format!("qwen:host:{id}"),
        provider_session_id: id.into(),
        provider: Provider::QwenCode,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: "YOLO task".into(),
        cwd: directory.path().to_owned(),
        state: SessionState::Completed,
        summary: "⚠ YOLO · YOLO task".into(),
        raw_state: Some("saved; YOLO".into()),
        pid: None,
        started_at: None,
        updated_at: None,
        pull_requests: None,
        capabilities: BTreeSet::new(),
    };

    controller.open(&session).unwrap();

    assert_eq!(
        fs::read_to_string(directory.path().join("invocations.log")).unwrap(),
        "--yolo --resume 11111111-2222-4333-8444-555555555555\n"
    );
}

#[test]
fn mistral_public_controller_correlates_exact_launch_then_discovers_and_opens_it() {
    let directory = private_tempdir();
    let vibe = directory.path().join("vibe");
    let server = directory.path().join("vibe-app-server");
    executable(
        &vibe,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "${1-}" = '--resume' ]; then
  printf 'resume %s\n' "$2" >> "$root/invocations.log"
  exit 0
fi
now=$(($(date +%s) * 1000))
cwd=$(pwd)
printf '{"id":"vibe-owned","title":"Vibe owned","preview":"owned task","status":{"type":"idle"},"createdAt":%s,"updatedAt":%s,"cwd":"%s","model":"devstral"}\n' "$now" "$now" "$cwd" > "$root/session.json"
printf 'launch model=%s prompt=%s\n' "${VIBE_ACTIVE_MODEL-}" "${1-}" >> "$root/invocations.log"
"##,
    );
    executable(
        &server,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"vibe-app-server","version":"test"},"capabilities":{}}}'
IFS= read -r initialized
IFS= read -r request
case "$request" in
  *'config/read'*)
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"config":{"models":[{"alias":"devstral"}]}}}'
    ;;
  *)
    if [ -f "$root/session.json" ]; then
      item=$(cat "$root/session.json")
      printf '{"jsonrpc":"2.0","id":2,"result":{"items":[%s]}}\n' "$item"
    else
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"items":[]}}'
    fi
    ;;
esac
"##,
    );
    let ownership = MistralVibeOwnership::load(directory.path().join("vibe-owned.json")).unwrap();
    let controller = MistralVibeController::host(
        vibe.display().to_string(),
        server.display().to_string(),
        ownership.clone(),
        directory.path().to_owned(),
    );
    assert_eq!(controller.available_models().unwrap(), vec!["devstral"]);
    let launched = controller
        .launch_foreground(&LaunchRequest {
            provider: Provider::MistralVibe,
            model: Some("devstral".into()),
            prompt: "owned task".into(),
            cwd: directory.path().to_owned(),
        })
        .unwrap();
    assert_eq!(
        launched.provider_session_hint.as_deref(),
        Some("vibe-owned")
    );

    let source = MistralVibeSource::host(server.display().to_string(), ownership);
    let sessions = source
        .discover(&DiscoveryRequest {
            include_completed: true,
            ..DiscoveryRequest::default()
        })
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].provider_session_id, "vibe-owned");
    controller.open(&sessions[0]).unwrap();
    let invocations = fs::read_to_string(directory.path().join("invocations.log")).unwrap();
    assert!(invocations.contains("launch model=devstral prompt=owned task"));
    assert!(invocations.contains("resume vibe-owned"));

    let mut external = sessions[0].clone();
    external.provider_session_id = "external".into();
    external.id = "mistral_vibe:host:external".into();
    assert!(controller.open(&external).is_err());
    assert!(controller.interrupt(&external).is_err());
}

#[test]
fn mistral_restarted_yolo_open_keeps_resume_argv_and_security_mode() {
    let directory = private_tempdir();
    let vibe = directory.path().join("vibe");
    executable(
        &vibe,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' "$*" >> "$root/invocations.log"
exit 0
"##,
    );
    let id = "vibe-yolo";
    let state = directory.path().join("vibe-owned.json");
    fs::write(
        &state,
        format!(
            r#"[{{"sessionId":"{id}","cwd":"{}","createdAtMs":1,"name":"YOLO task","yolo":true}}]"#,
            directory.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = MistralVibeOwnership::load(state).unwrap();
    let controller = MistralVibeController::host(
        vibe.display().to_string(),
        "unused-app-server",
        ownership,
        directory.path().to_owned(),
    );
    let session = AgentSession {
        id: format!("mistral_vibe:host:{id}"),
        provider_session_id: id.into(),
        provider: Provider::MistralVibe,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: "YOLO task".into(),
        cwd: directory.path().to_owned(),
        state: SessionState::Completed,
        summary: "⚠ YOLO · YOLO task".into(),
        raw_state: Some("idle; YOLO".into()),
        pid: None,
        started_at: None,
        updated_at: None,
        pull_requests: None,
        capabilities: BTreeSet::new(),
    };

    controller.open(&session).unwrap();

    assert_eq!(
        fs::read_to_string(directory.path().join("invocations.log")).unwrap(),
        "--auto-approve --resume vibe-yolo\n"
    );
}

#[test]
fn qwen_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("qwen-reentry") {
        run_qwen_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer(
        "qwen-reentry",
        "qwen_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty",
        &["⚠ YOLO MODE · Qwen Code"],
        "QWEN_REENTRY_DONE",
    );
}

#[test]
fn qwen_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("qwen-legacy-reentry") {
        run_qwen_legacy_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer_checked(
        "qwen-legacy-reentry",
        "qwen_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty",
        &[],
        &["⚠ YOLO MODE · Qwen Code"],
        "QWEN_LEGACY_REENTRY_DONE",
    );
}

#[test]
fn mistral_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("mistral-legacy-reentry") {
        run_mistral_legacy_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer_checked(
        "mistral-legacy-reentry",
        "mistral_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty",
        &[],
        &["⚠ YOLO MODE · Mistral Vibe"],
        "MISTRAL_LEGACY_REENTRY_DONE",
    );
}

#[test]
fn mistral_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("mistral-reentry") {
        run_mistral_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer(
        "mistral-reentry",
        "mistral_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty",
        &["⚠ YOLO MODE · Mistral Vibe"],
        "MISTRAL_REENTRY_DONE",
    );
}

#[test]
fn shared_restarted_yolo_open_observes_resume_argv_and_warning_for_each_harness_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("shared-reentry") {
        run_shared_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer(
        "shared-reentry",
        "shared_restarted_yolo_open_observes_resume_argv_and_warning_for_each_harness_in_a_real_pty",
        &[
            "⚠ YOLO MODE · Oh My Pi",
            "⚠ YOLO MODE · Grok",
            "⚠ YOLO MODE · Kilo Code",
            "⚠ YOLO MODE · OpenHands",
            "⚠ YOLO MODE · Hermes Agent",
        ],
        "SHARED_REENTRY_DONE",
    );
}

#[test]
fn shared_legacy_reentry_uses_safe_resume_without_warning_for_each_harness_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("shared-legacy-reentry") {
        run_shared_legacy_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer_checked(
        "shared-legacy-reentry",
        "shared_legacy_reentry_uses_safe_resume_without_warning_for_each_harness_in_a_real_pty",
        &[],
        &[
            "⚠ YOLO MODE · Oh My Pi",
            "⚠ YOLO MODE · Grok",
            "⚠ YOLO MODE · Kilo Code",
            "⚠ YOLO MODE · OpenHands",
            "⚠ YOLO MODE · Hermes Agent",
        ],
        "SHARED_LEGACY_REENTRY_DONE",
    );
}

#[test]
fn antigravity_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("antigravity-reentry") {
        run_antigravity_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer(
        "antigravity-reentry",
        "antigravity_restarted_yolo_open_observes_resume_argv_and_warning_in_a_real_pty",
        &["⚠ YOLO MODE · Antigravity"],
        "ANTIGRAVITY_REENTRY_DONE",
    );
}

#[test]
fn antigravity_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty() {
    if std::env::var(PTY_CHILD).as_deref() == Ok("antigravity-legacy-reentry") {
        run_antigravity_legacy_reentry_pty_child();
        return;
    }
    run_reentry_pty_outer_checked(
        "antigravity-legacy-reentry",
        "antigravity_legacy_reentry_uses_safe_resume_without_warning_in_a_real_pty",
        &[],
        &["⚠ YOLO MODE · Antigravity"],
        "ANTIGRAVITY_LEGACY_REENTRY_DONE",
    );
}

fn run_mistral_pty_child() {
    let _cleanup = NativeSessionCleanup;
    let directory = private_tempdir();
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let vibe = directory.path().join("vibe");
    let server = directory.path().join("vibe-app-server");
    executable(
        &vibe,
        r##"#!/usr/bin/env bash
set -euo pipefail
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [[ "${1:-}" == --resume ]]; then
  [[ "${2:-}" == vibe-owned ]]
  printf 'VIBE_RESUME_EXACT\n'
  sleep 1
  exit 0
fi
now=$(($(date +%s) * 1000))
printf '{"id":"vibe-owned","title":"Vibe owned","preview":"owned task","status":{"type":"idle"},"createdAt":%s,"updatedAt":%s,"cwd":"%s","model":"devstral"}\n' "$now" "$now" "$(pwd)" > "$root/session.json"
stty raw -echo
printf '\033[2J\033[HVIBE_NATIVE_READY'
exec sleep 60
"##,
    );
    executable(
        &server,
        r##"#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"vibe-app-server","version":"test"},"capabilities":{}}}'
IFS= read -r initialized
IFS= read -r request
case "$request" in
  *'config/read'*)
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"config":{"models":[{"alias":"devstral"}]}}}'
    ;;
  *)
    if [ -f "$root/session.json" ]; then
      item=$(cat "$root/session.json")
      printf '{"jsonrpc":"2.0","id":2,"result":{"items":[%s]}}\n' "$item"
    else
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"items":[]}}'
    fi
    ;;
esac
"##,
    );
    let ownership = MistralVibeOwnership::load(directory.path().join("owned.json")).unwrap();
    let source = MistralVibeSource::host(server.display().to_string(), ownership.clone());
    let controller = MistralVibeController::host(
        vibe.display().to_string(),
        server.display().to_string(),
        ownership,
        workspace.clone(),
    );
    let request = LaunchRequest {
        provider: Provider::MistralVibe,
        model: Some("devstral".into()),
        prompt: "owned task".into(),
        cwd: workspace,
    };
    let launched = controller.launch_foreground(&request).unwrap();
    assert_eq!(
        launched.provider_session_hint.as_deref(),
        Some("vibe-owned")
    );
    let sessions = source
        .discover(&DiscoveryRequest {
            include_completed: true,
            ..DiscoveryRequest::default()
        })
        .unwrap();
    let mut snapshot = SessionSnapshot {
        sessions,
        ..SessionSnapshot::default()
    };
    controller.enrich(&mut snapshot);
    let session = only_live(snapshot);
    println!("VIBE_LAUNCH_RETURNED");
    println!("VIBE_REATTACHING");
    thread::sleep(Duration::from_millis(150));
    controller.open(&session).unwrap();
    println!("VIBE_REATTACHED");
    controller.interrupt(&session).unwrap();
    controller.open(&session).unwrap();
    println!("VIBE_CONTROLLER_OK");
}

fn run_qwen_pty_child() {
    let _cleanup = NativeSessionCleanup;
    let directory = private_tempdir();
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let qwen = directory.path().join("qwen");
    executable(
        &qwen,
        r##"#!/usr/bin/env bash
set -euo pipefail
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [[ "${1:-} ${2:-}" == 'sessions list' ]]; then
  [[ ! -f "$root/history.jsonl" ]] || cat "$root/history.jsonl"
  exit 0
fi
if [[ "${1:-} ${2:-}" == 'sessions ps' ]]; then
  [[ ! -f "$root/live.jsonl" ]] || cat "$root/live.jsonl"
  exit 0
fi
if [[ "${1:-}" == --resume ]]; then
  printf 'QWEN_RESUME_EXACT\n'
  sleep 1
  exit 0
fi
session=''
prompt=''
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --session-id) session=$2; shift 2 ;;
    --model) shift 2 ;;
    --prompt-interactive) prompt=$2; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "$session" && -n "$prompt" ]]
now=$(($(date +%s) * 1000))
cwd=$(pwd)
printf '{"sessionId":"%s","startTime":"2026-08-25T00:00:00Z","mtime":%s,"prompt":"owned task","customTitle":"Qwen owned","cwd":"%s"}\n' "$session" "$now" "$cwd" > "$root/history.jsonl"
printf '{"pid":%s,"sessionId":"%s","cwd":"%s","name":"Qwen owned","startedAt":%s}\n' "$$" "$session" "$cwd" "$now" > "$root/live.jsonl"
trap 'rm -f "$root/live.jsonl"; exit 0' TERM INT EXIT
stty raw -echo
printf '\033[2J\033[HQWEN_NATIVE_READY'
exec sleep 60
"##,
    );
    let ownership = QwenOwnership::load(directory.path().join("owned.json")).unwrap();
    let source = QwenSource::host(qwen.display().to_string(), ownership.clone());
    let controller = QwenController::host(qwen.display().to_string(), ownership);
    let request = LaunchRequest {
        provider: Provider::QwenCode,
        model: Some("qwen3-coder-plus".into()),
        prompt: "owned task".into(),
        cwd: workspace,
    };
    let launched = controller.launch_foreground(&request).unwrap();
    let id = launched.provider_session_hint.unwrap();
    let sessions = source.discover(&DiscoveryRequest::default()).unwrap();
    let mut snapshot = SessionSnapshot {
        sessions,
        ..SessionSnapshot::default()
    };
    controller.enrich(&mut snapshot);
    let session = only_live(snapshot);
    assert_eq!(session.provider_session_id, id);
    println!("QWEN_LAUNCH_RETURNED");
    println!("QWEN_REATTACHING");
    thread::sleep(Duration::from_millis(150));
    controller.open(&session).unwrap();
    println!("QWEN_REATTACHED");
    controller.interrupt(&session).unwrap();
    controller.open(&session).unwrap();
    println!("QWEN_CONTROLLER_OK");
}

fn run_qwen_reentry_pty_child() {
    let directory = private_tempdir();
    let qwen = directory.path().join("qwen");
    let invocations = directory.path().join("invocations.log");
    executable(
        &qwen,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf QWEN_REENTRY_PROVIDER\n",
            invocations.display()
        ),
    );
    let id = "11111111-2222-4333-8444-555555555555";
    let state = directory.path().join("qwen-owned.json");
    fs::write(
        &state,
        format!(
            r#"[{{"sessionId":"{id}","cwd":"{}","createdAtMs":1,"name":"YOLO task","yolo":true}}]"#,
            directory.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = QwenOwnership::load(state).unwrap();
    let controller = QwenController::host(qwen.display().to_string(), ownership);
    controller
        .open(&completed_session(
            &format!("qwen:host:{id}"),
            id,
            Provider::QwenCode,
            directory.path(),
        ))
        .unwrap();
    assert_eq!(
        fs::read_to_string(invocations).unwrap(),
        "--yolo --resume 11111111-2222-4333-8444-555555555555\n"
    );
    println!("QWEN_REENTRY_DONE");
}

fn run_qwen_legacy_reentry_pty_child() {
    let directory = private_tempdir();
    let qwen = directory.path().join("qwen");
    let invocations = directory.path().join("invocations.log");
    executable(
        &qwen,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf QWEN_LEGACY_REENTRY_PROVIDER\n",
            invocations.display()
        ),
    );
    let id = "11111111-2222-4333-8444-555555555555";
    let state = directory.path().join("qwen-owned.json");
    fs::write(
        &state,
        format!(
            r#"[{{"sessionId":"{id}","cwd":"{}","createdAtMs":1,"name":"Legacy task"}}]"#,
            directory.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = QwenOwnership::load(state).unwrap();
    let controller = QwenController::host(qwen.display().to_string(), ownership);
    controller
        .open(&completed_session(
            &format!("qwen:host:{id}"),
            id,
            Provider::QwenCode,
            directory.path(),
        ))
        .unwrap();
    assert_eq!(
        fs::read_to_string(invocations).unwrap(),
        "--resume 11111111-2222-4333-8444-555555555555\n"
    );
    println!("QWEN_LEGACY_REENTRY_DONE");
}

fn run_mistral_reentry_pty_child() {
    run_mistral_reentry_pty_child_with_security(true);
}

fn run_mistral_legacy_reentry_pty_child() {
    run_mistral_reentry_pty_child_with_security(false);
}

fn run_mistral_reentry_pty_child_with_security(yolo: bool) {
    let directory = private_tempdir();
    let vibe = directory.path().join("vibe");
    let invocations = directory.path().join("invocations.log");
    executable(
        &vibe,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf MISTRAL_REENTRY_PROVIDER\n",
            invocations.display()
        ),
    );
    let id = "vibe-yolo";
    let state = directory.path().join("vibe-owned.json");
    fs::write(
        &state,
        format!(
            r#"[{{"sessionId":"{id}","cwd":"{}","createdAtMs":1,"name":"task"{}}}]"#,
            directory.path().display(),
            if yolo { r#","yolo":true"# } else { "" }
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = MistralVibeOwnership::load(state).unwrap();
    let controller = MistralVibeController::host(
        vibe.display().to_string(),
        "unused-app-server",
        ownership,
        directory.path().to_owned(),
    );
    controller
        .open(&completed_session(
            &format!("mistral_vibe:host:{id}"),
            id,
            Provider::MistralVibe,
            directory.path(),
        ))
        .unwrap();
    assert_eq!(
        fs::read_to_string(invocations).unwrap(),
        if yolo {
            "--auto-approve --resume vibe-yolo\n"
        } else {
            "--resume vibe-yolo\n"
        }
    );
    println!(
        "{}",
        if yolo {
            "MISTRAL_REENTRY_DONE"
        } else {
            "MISTRAL_LEGACY_REENTRY_DONE"
        }
    );
}

fn run_shared_reentry_pty_child() {
    run_shared_reentry_pty_child_with_security(true);
}

fn run_shared_legacy_reentry_pty_child() {
    run_shared_reentry_pty_child_with_security(false);
}

fn run_shared_reentry_pty_child_with_security(yolo: bool) {
    let directory = private_tempdir();
    let native_executable = directory.path().join("native");
    let invocations = directory.path().join("invocations.log");
    executable(
        &native_executable,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf SHARED_REENTRY_PROVIDER\n",
            invocations.display()
        ),
    );
    let cases = [
        (
            Provider::OhMyPi,
            "session-id",
            "--yolo --resume session-id",
            "--resume session-id",
        ),
        (
            Provider::Grok,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--yolo --no-auto-update --resume aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--no-auto-update --resume aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ),
        (
            Provider::KiloCode,
            "session-id",
            "--yolo --session session-id",
            "--session session-id",
        ),
        (
            Provider::OpenHands,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--always-approve --resume aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--resume aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        ),
        (
            Provider::Hermes,
            "12345678_123456_abcdef",
            "--yolo chat --cli --resume 12345678_123456_abcdef",
            "chat --cli --resume 12345678_123456_abcdef",
        ),
    ];
    for (index, (provider, session_id, yolo_expected, safe_expected)) in
        cases.into_iter().enumerate()
    {
        let state = directory.path().join(format!("owned-{index}.json"));
        fs::write(
            &state,
            format!(
                r#"[{{"sessionId":"{session_id}","cwd":"{}","createdAtMs":1,"name":"task"{}}}]"#,
                directory.path().display(),
                if yolo { r#","yolo":true"# } else { "" }
            ),
        )
        .unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        let ownership = SessionMigrateNativeOwnership::load(provider.clone(), state).unwrap();
        let controller = SessionMigrateNativeController::host(
            provider.clone(),
            native_executable.display().to_string(),
            directory.path().to_owned(),
            ownership,
        )
        .unwrap();
        controller
            .open(&completed_session(
                &format!("shared-reentry-{index}"),
                session_id,
                provider,
                directory.path(),
            ))
            .unwrap();
        let line = fs::read_to_string(&invocations)
            .unwrap()
            .lines()
            .nth(index)
            .map(str::to_owned);
        assert_eq!(
            line.as_deref(),
            Some(if yolo { yolo_expected } else { safe_expected })
        );
    }
    println!(
        "{}",
        if yolo {
            "SHARED_REENTRY_DONE"
        } else {
            "SHARED_LEGACY_REENTRY_DONE"
        }
    );
}

fn run_antigravity_reentry_pty_child() {
    run_antigravity_reentry_pty_child_with_security(true);
}

fn run_antigravity_legacy_reentry_pty_child() {
    run_antigravity_reentry_pty_child_with_security(false);
}

fn run_antigravity_reentry_pty_child_with_security(yolo: bool) {
    let directory = private_tempdir();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let agy = directory.path().join("antigravity");
    let invocations = directory.path().join("invocations.log");
    executable(
        &agy,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf ANTIGRAVITY_REENTRY_PROVIDER\n",
            invocations.display()
        ),
    );
    let state = directory.path().join("sessions.json");
    fs::write(
        &state,
        format!(
            r#"[{{"workspace":"{}","conversationId":"owned","createdAtMs":1{}}}]"#,
            workspace.display(),
            if yolo { r#","yolo":true"# } else { "" }
        ),
    )
    .unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    let ownership = AntigravityOwnership::load(state).unwrap();
    let controller = AntigravityController::managed(agy.display().to_string(), ownership).unwrap();
    controller
        .open(&completed_session(
            "antigravity:host:owned",
            "owned",
            Provider::Antigravity,
            &workspace,
        ))
        .unwrap();
    assert_eq!(
        fs::read_to_string(invocations).unwrap(),
        if yolo {
            "--dangerously-skip-permissions --conversation owned\n"
        } else {
            "--conversation owned\n"
        }
    );
    println!(
        "{}",
        if yolo {
            "ANTIGRAVITY_REENTRY_DONE"
        } else {
            "ANTIGRAVITY_LEGACY_REENTRY_DONE"
        }
    );
}

fn completed_session(
    id: &str,
    provider_session_id: &str,
    provider: Provider,
    cwd: &Path,
) -> AgentSession {
    AgentSession {
        id: id.into(),
        provider_session_id: provider_session_id.into(),
        provider,
        runtime: Runtime::Host,
        kind: SessionKind::Managed,
        name: "YOLO task".into(),
        cwd: cwd.to_owned(),
        state: SessionState::Completed,
        summary: "⚠ YOLO · YOLO task".into(),
        raw_state: Some("saved; YOLO".into()),
        pid: None,
        started_at: None,
        updated_at: None,
        pull_requests: None,
        capabilities: BTreeSet::new(),
    }
}

fn run_reentry_pty_outer(provider: &str, test_name: &str, warnings: &[&str], done: &str) {
    run_reentry_pty_outer_checked(provider, test_name, warnings, &[], done);
}

fn run_reentry_pty_outer_checked(
    provider: &str,
    test_name: &str,
    warnings: &[&str],
    forbidden: &[&str],
    done: &str,
) {
    let (mut master, slave) = outer_pty();
    set_nonblocking(&master);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(PTY_CHILD, provider)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ChildGuard::new(command.spawn().unwrap());
    let mut output = Vec::new();
    read_until(
        &mut master,
        &mut output,
        done.as_bytes(),
        Duration::from_secs(5),
    );
    let text = String::from_utf8_lossy(&output);
    for warning in warnings {
        assert!(text.contains(warning), "missing {warning:?}: {text}");
    }
    for warning in forbidden {
        assert!(!text.contains(warning), "unexpected {warning:?}: {text}");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(status.success(), "{text}");
            child.reaped = true;
            break;
        }
        assert!(Instant::now() < deadline, "re-entry child did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

fn only_live(snapshot: SessionSnapshot) -> open_agent_view::domain::AgentSession {
    assert_eq!(snapshot.sessions.len(), 1);
    let session = snapshot.sessions.into_iter().next().unwrap();
    assert_eq!(session.state, SessionState::Working);
    assert!(session
        .capabilities
        .contains(&open_agent_view::domain::Capability::Interrupt));
    session
}

fn run_pty_outer(provider: &str, test_name: &str, marker: &str, ready: &[u8]) {
    let (mut master, slave) = outer_pty();
    set_nonblocking(&master);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(PTY_CHILD, provider)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ChildGuard::new(command.spawn().unwrap());
    let mut output = Vec::new();
    read_until(&mut master, &mut output, ready, Duration::from_secs(5));
    master.write_all(b"\x1b[1;2D").unwrap();
    read_until_present(
        &mut master,
        &mut output,
        format!("{marker}_LAUNCH_RETURNED").as_bytes(),
        Duration::from_secs(5),
    );
    read_until_present(
        &mut master,
        &mut output,
        format!("{marker}_REATTACHING").as_bytes(),
        Duration::from_secs(5),
    );
    read_until(&mut master, &mut output, ready, Duration::from_secs(5));
    master.write_all(b"\x1b[1;2C").unwrap();
    read_until_present(
        &mut master,
        &mut output,
        format!("{marker}_REATTACHED").as_bytes(),
        Duration::from_secs(5),
    );
    read_until_present(
        &mut master,
        &mut output,
        format!("{marker}_RESUME_EXACT").as_bytes(),
        Duration::from_secs(5),
    );
    read_until_present(
        &mut master,
        &mut output,
        format!("{marker}_CONTROLLER_OK").as_bytes(),
        Duration::from_secs(5),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(status.success(), "{}", String::from_utf8_lossy(&output));
            child.reaped = true;
            break;
        }
        assert!(Instant::now() < deadline, "controller child did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

struct NativeSessionCleanup;

impl Drop for NativeSessionCleanup {
    fn drop(&mut self) {
        open_agent_view::native_session::shutdown_all();
    }
}

struct ChildGuard {
    child: std::process::Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn outer_pty() -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let size_ptr = &mut size as *mut libc::winsize;
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            size_ptr,
        )
    };
    assert_eq!(result, 0);
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

fn set_nonblocking(file: &File) {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
}

fn read_until(master: &mut File, output: &mut Vec<u8>, needle: &[u8], timeout: Duration) {
    let previous = occurrences(output, needle);
    let deadline = Instant::now() + timeout;
    let mut bytes = [0_u8; 4096];
    loop {
        match master.read(&mut bytes) {
            Ok(0) => {}
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("failed to read outer PTY: {error}"),
        }
        if occurrences(output, needle) > previous {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not observe {:?}: {}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(output)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_until_present(master: &mut File, output: &mut Vec<u8>, needle: &[u8], timeout: Duration) {
    if occurrences(output, needle) != 0 {
        return;
    }
    read_until(master, output, needle, timeout);
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}
