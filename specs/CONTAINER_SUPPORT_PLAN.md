# FlightDeck — Container Support Implementation Plan

Status: **Draft for review** · Target: v1 of containerized agent execution
Reference PoC: DAC (`.reference-developer-agent-container/`, gitignored)

This plan adds the ability to run agents inside isolated rootless **Podman**
containers instead of directly on the host, controlled by a single project-wide
toggle. It is written to slot into FlightDeck's existing architecture (SPECS
§3–§30) and trait seams (SPECS §26–§27) with the smallest possible blast radius.

---

## 0. Locked requirements (from the requirements interview)

| Area | Decision |
| --- | --- |
| Workspace | **Bind-mount** the host git worktree into the container at `/workspace` with `--userns keep-id`. Host keeps owning the worktree + all git ops. No diff/apply/sync layer. |
| Opt-in | **Single project-wide toggle** (`[execution] enabled`). All agents containerized or none. |
| Runtime | **Podman only**, behind a trait so Docker can be added later. |
| Child shells (Ctrl-t) | Run **inside the same container** via `podman exec`. |
| Sidecars / MCP | **None in v1.** Single container per tab. Pod model deferred. |
| Security | Adopt DAC's **full, non-disableable** guardrails. |
| Network | **Full outbound** in v1. Egress allowlist is a *later* goal — leave room. |
| Lifecycle | **Persist & reattach**: containers survive a FlightDeck restart; reattach the PTY to a still-running container; exited ones recover as metadata only (manual `Ctrl-r`). |
| Auth | Configurable: mount host creds read-only **or** inject an allowlisted API-key env var. |
| Ports | Loopback publish (`127.0.0.1:<port>`): configured list (v1) + ad-hoc command (fast-follow). |
| Resource limits | Configurable `cpu`/`memory`/`pids` with DAC-like defaults. *(Default-on vs off: confirm — see §13.)* |
| Image | **FlightDeck-managed base + project customization** (see §8). |

---

## 1. Key design insight — the `PtyBackend` seam does not change

FlightDeck already routes every terminal spawn through one trait
(`src/contracts/traits.rs`):

```rust
trait PtyBackend {
    fn spawn(&self, cmd: &str, args: &[String], cwd: &Path, size: PtySize)
        -> Result<Box<dyn PtySession>>;
}
```

Running an agent in a container is just **a different command line** handed to
that same backend. All three interactive operations are plain `podman`
invocations the existing `PortablePtyBackend` can run in a PTY unchanged:

| Operation | argv handed to `PtyBackend::spawn` |
| --- | --- |
| Start primary agent (fresh) | `podman run -it --name flightdeck-<id> … <image> <agent-cmd> <agent-args>` |
| Reattach primary (after restart) | `podman attach flightdeck-<id>` |
| Child shell (Ctrl-t) | `podman exec -it flightdeck-<id> <shell>` |

So the work is **not** a new PTY backend. It is:

1. A **pure arg-builder module** that turns a tab + config into those argv
   vectors (mirroring DAC's pure `buildRunArgs`), plus a **guardrails** function
   (mirroring DAC's `guards.ts`). No Podman needed to unit-test either.
2. A **`ContainerRuntime` trait** for the non-interactive *control-plane* calls
   (build image, inspect/remove container, list) — behind a seam with a fake,
   exactly like `GitExecutor`/`FileSystem` (SPECS §27).
3. **Branching at the existing spawn sites** in `app/state.rs` to choose the
   local launch vs. the container launch.

This keeps `terminal/`, the VT parser, status detection, selection, rendering,
and the whole event loop untouched.

### 1.1 Data plane vs. control plane

- **Data plane (interactive, needs a PTY):** `run` / `attach` / `exec` → built
  by `runtime::container` and spawned through the existing `PtyBackend`.
- **Control plane (non-interactive `podman` subcommands):** `image exists`,
  `build`, `inspect` (container state), `rm`, `ps --filter` → behind the new
  `ContainerRuntime` trait. Real impl shells out with `std::process::Command`;
  slow calls (`build`) run on a background worker like worktree materialization.

### 1.2 Deterministic container name = stable identity

Name every container `flightdeck-<sanitized tab.id>` and label it
`flightdeck.tab=<id>` / `flightdeck.repo=<hash>`. Because the name is derivable
from the persisted `TabState.id`, we need **no runtime id captured at spawn**:
child-shell `exec`, reattach, and teardown all reconstruct the name from the
tab. This is what makes persist-&-reattach and label-based reconcile simple.

---

## 2. Module layout (new + touched)

```
src/runtime/                      NEW — all container logic
  mod.rs
  spec.rs        ContainerSpec + builders' input types (pure data)
  container.rs   build_run_args / build_attach_args / build_exec_args (PURE)
  guards.rs      enforce_guardrails(&[String]) -> Result<()>  (PURE)
  image.rs       image tag computation + build orchestration (uses ContainerRuntime)
  name.rs        container_name(tab_id) / labels (PURE)

src/contracts/
  traits.rs      + trait ContainerRuntime
  real.rs        + PodmanCli : ContainerRuntime (shells out)
  domain.rs      + ExecutionConfig; + TabState container fields
  error.rs       + container error variants

src/config/
  schema.rs      validate [execution]
  init.rs        wizard/default writes [execution] (disabled by default)

src/app/state.rs SPAWN-SITE BRANCHING + container teardown + reattach
src/testing/     + FakeContainerRuntime
src/lib.rs       construct PodmanCli, add to Services; reattach in resume path
tests/           + container_args.rs, container_guards.rs (pure); ignored e2e
containers/      NEW — shipped reference Containerfiles (Containerfile.claude, …)
```

Proposed spec home: a new **SPECS §31 "Container execution"** section capturing
the invariants below, so subagents have an authoritative contract (matching how
PLAN.md delegates against SPECS).

---

## 3. The pure arg-builder (`runtime/container.rs`, `runtime/spec.rs`)

```rust
/// Everything needed to launch/attach/exec, resolved on the UI thread from
/// (AgentDef, ExecutionConfig, TabState, repo_root). Owned + Send.
pub struct ContainerSpec {
    pub name: String,            // flightdeck-<id>
    pub labels: Vec<(String, String)>,
    pub image: String,
    pub workspace_host: PathBuf, // absolute worktree path on host
    pub agent_cmd: String,
    pub agent_args: Vec<String>,
    pub limits: Limits,          // cpu / memory / pids
    pub forward_ports: Vec<u16>,
    pub auth: AuthMounts,        // resolved mounts + env keys
    pub host_uid: u32,           // for --userns keep-id / --user
}

/// `podman run` argv (everything after the binary name). PURE.
pub fn build_run_args(spec: &ContainerSpec) -> Vec<String>;
/// `podman attach <name>` argv. PURE.
pub fn build_attach_args(name: &str) -> Vec<String>;
/// `podman exec -it <name> <shell>` argv. PURE.
pub fn build_exec_args(name: &str, shell_cmd: &str, shell_args: &[String]) -> Vec<String>;
```

`build_run_args` emits, in order (DAC parity):

```
run -it --rm
  --name flightdeck-<id>
  --label flightdeck.tab=<id> --label flightdeck.repo=<hash>
  --cap-drop all --security-opt no-new-privileges
  --cpus <n> --memory <m> --pids-limit <p>
  --userns keep-id --user <host_uid>
  --workdir /workspace
  --volume <workspace_host>:/workspace        # mount flags: see §11
  [--volume <hostcreds>:<ctr_path>:ro]*        # auth: mount mode
  [--env KEY=<value-from-host-allowlist>]*     # auth: env mode (discrete argv)
  [--publish 127.0.0.1:<port>:<port>]*         # forward_ports
  --env FLIGHTDECK=1 --env FLIGHTDECK_TAB=<id>
  <image>
  <agent_cmd> <agent_args...>
```

Notes:
- **`--rm` is intentional and compatible with reattach** (see §7): it only
  removes on *container exit*, not on client detach.
- Secrets are never shell-interpolated — Rust argv elements are inherently
  discrete, so this property is free (DAC has to work for it).
- Network is left default in v1 (no `--network` flag). The egress proxy would
  later insert a sidecar + `http_proxy` envs here; the builder leaves a seam.

These functions are the **only** place `podman run/attach/exec` argv is
constructed, and they are trivially unit-tested (assert on the vector) without a
container runtime — exactly how DAC tests `builder.test.ts` (21 tests).

---

## 4. Guardrails (`runtime/guards.rs`)

Port DAC's `guards.ts` verbatim in spirit — a pure function run on the built
argv immediately before any `run` spawn:

```rust
pub fn enforce_guardrails(args: &[String]) -> Result<()>;
```

Non-disableable rejections:
1. `--privileged`
2. container-socket mounts (`docker.sock`, `/run/podman`, `DOCKER_HOST`)
3. `--env-host`
4. mounting `$HOME` itself (canonicalize both sides; subdir mounts allowed)
5. any `--publish` not bound to `127.0.0.1`

Enforcement is belt-and-suspenders: our own builder never emits these, but
guardrails defend against future config-driven mounts (auth paths, custom
Containerfile `workspace_path`-style entries) and regressions.

**Test scanner (mirrors the SPECS §5 `rebase` scanner in `tests/guards.rs`):**
add a source-scan test asserting the strings `"podman"`/`"run"` argv is only
assembled in `src/runtime/`, so no other module can hand-roll an unguarded
`podman run`.

---

## 5. Config additions (`domain.rs` + `config/schema.rs`)

```rust
/// `[execution]` config section. Absent table → Default (disabled) → today's
/// behaviour is preserved bit-for-bit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default)]                 pub enabled: bool,           // master toggle, OFF by default
    #[serde(default = "default_runtime")] pub runtime: String,    // "podman"
    #[serde(default)]                 pub image: Option<String>,  // resolved image, or None → built
    #[serde(default)]                 pub base_image: Option<String>, // override flightdeck base
    #[serde(default)]                 pub packages: Vec<String>,  // declarative customization
    #[serde(default)]                 pub setup_script: Option<String>,
    #[serde(default)]                 pub containerfile: Option<String>, // advanced escape hatch
    #[serde(default)]                 pub forward_ports: Vec<u16>,
    #[serde(default)]                 pub limits: Limits,         // cpu/memory/pids (+defaults)
    #[serde(default)]                 pub auth: AuthConfig,       // mount creds | env keys
}
```

`Limits { cpu: f32=4, memory: String="8g", pids: u32=512 }`,
`AuthConfig { mounts: Vec<AuthMount>, env_allow: Vec<String> }`.

Add `#[serde(default)] pub execution: ExecutionConfig` to `Config`. Every field
is `#[serde(default)]` ⇒ existing `config.toml` files keep working untouched.

`config/schema.rs::validate` adds: runtime ∈ {podman}; ports unique & non-zero;
`containerfile` xor `packages/setup_script` (advanced vs declarative); auth mount
paths relative-or-absolute but never `$HOME` root (defer hard check to guardrails).

---

## 6. State additions (`domain.rs::TabState`)

Persist just enough to reattach/teardown correctly after a restart, all additive
and `#[serde(default)]` for backward compatibility:

```rust
#[serde(default)] pub containerized: bool,        // was this tab launched in a container
#[serde(default)] pub container_image: Option<String>, // image actually used (provenance)
```

The container **name** is derived from `id` (no need to store it). `containerized`
is recorded per-tab so that toggling `[execution] enabled` off later does not
mislead reattach/teardown of tabs that *were* containerized.

---

## 7. Lifecycle: persist & reattach

### 7.1 The critical spike (do this FIRST — §12 Phase 0)

Persist-&-reattach hinges on one Podman behaviour that must be proven on macOS
before building on it:

> When FlightDeck (the process holding the `podman run -it` client) is killed,
> does the container keep running under `conmon`, and can a fresh
> `podman attach flightdeck-<id>` reconnect a working PTY (input, output,
> SIGWINCH resize, Ctrl-C)?

Expected: yes — `conmon` keeps the container alive independent of the client;
`--rm` removes it only when the *container's* main process exits. So:

- still-running on restart → `podman attach` reconnects, tab is live again;
- exited while FlightDeck was down → `--rm` already removed it → tab recovers as
  "session lost", manual `Ctrl-r` (consistent with the never-auto-relaunch
  invariant, SPECS §10).

If the spike disproves clean attach, fall back to **named pipe / detached run +
`podman logs --follow` + `podman attach`** or drop `--rm` and remove containers
ourselves on teardown. Resolve before Phase 2.

### 7.2 Spawn-site changes (`app/state.rs`)

A small resolver decides the launch form. Replace the direct `build_launch`
calls at the three sites with a branch:

- `finalize_new_tab` & `start_primary_for` (primary spawn):
  - if `!execution.enabled` → today's `build_launch` (unchanged).
  - else → build `ContainerSpec`, `enforce_guardrails`, then
    `session.spawn_primary(pty, "podman", build_run_args(&spec), host_cwd, size)`.
    Set `tab.meta.containerized = true`, record image.
- `cmd_new_child` (Ctrl-t): if the tab is containerized →
  `spawn_child(pty, "podman", build_exec_args(name, shell, args), cwd, size)`;
  else today's `shell_launch`.
- `resume_agents` (restart path): if the tab is containerized and
  `container.state(name) == Running` → spawn `podman attach` (reattach); if
  `Exited`/absent → leave as session-lost (no auto-spawn). Non-containerized →
  today's behaviour.

The `cwd` passed to `PtyBackend` for container forms is just where the `podman`
client runs (host worktree) — harmless; the agent's real cwd is `/workspace`.

### 7.3 Teardown (container removal)

`PtySession::terminate_tree` kills the local `podman` client but may leave the
container running. So containerized teardown needs an explicit
`container.remove(name, force=true)` after `session.terminate_all()`, added to
the three lifecycle exits in `app/state.rs`:
`cmd_close_tab` (force path), `cmd_abandon`, `cmd_finish_merge`. With `--rm`,
removal on Ctrl-C exit is automatic; the explicit `rm -f` covers the
force/abandon paths and orphan cleanup. Keep `PtySession` execution-agnostic —
do removal at the app layer through `services.container`.

### 7.4 Startup reconcile

In `lib.rs` startup (alongside `recover`): `container.list(label=flightdeck.repo=<hash>)`,
and for any running container whose tab is gone, leave it for the user (or offer
cleanup) — do **not** auto-kill. This mirrors DAC's `reconcileStaleSessions`.

---

## 8. Image strategy (`runtime/image.rs`) — managed base + customization

Recommended model (agreed direction: FlightDeck handles building, user controls
dependencies):

1. **FlightDeck-owned base images.** Ship `containers/Containerfile.<agent>`
   installing the agent CLI + git + a non-root `agent` user wired for UID
   mapping. Tag `flightdeck/<agent>-base:<flightdeck-version>`.
2. **Project customization layer**, two tiers:
   - *Declarative (default):* `packages` + `setup_script` in `[execution]`.
     FlightDeck generates a final Containerfile (`FROM flightdeck/<agent>-base` +
     `apt/apk install <packages>` + `RUN <setup_script>`) and builds it as
     `flightdeck/<repo-hash>-<agent>:local`.
   - *Advanced:* `containerfile = "containers/agent.Containerfile"` — the user's
     own file (expected to `FROM` a flightdeck base). Built as-is.
3. **Staleness via label.** Hash the customization inputs (base tag + packages +
   setup-script bytes + Containerfile bytes) into an image label
   `flightdeck.build=<hash>`. Before launch, compare; rebuild if changed.
4. **Build orchestration.** `flightdeck image build [--force]` and an
   auto-build-on-first-launch run on the **existing background-worker pattern**
   (`std::thread` + mpsc, exactly like `WorktreeJob`/`spawn_worktree_job`), so
   the UI never blocks on a multi-minute build. Progress streamed to a
   transient overlay.

`ContainerRuntime::build_image(tag, containerfile, context)` does the shell-out;
`image.rs` owns tag/hash logic (pure, testable) and the generated-Containerfile
templating.

---

## 9. The `ContainerRuntime` trait (control plane)

```rust
// contracts/traits.rs
pub trait ContainerRuntime {
    fn available(&self) -> Result<()>;                       // podman on PATH + machine up
    fn image_exists(&self, tag: &str) -> Result<bool>;
    fn image_label(&self, tag: &str, key: &str) -> Result<Option<String>>;
    fn build_image(&self, tag: &str, containerfile: &Path, context: &Path) -> Result<()>;
    fn container_state(&self, name: &str) -> Result<ContainerState>; // Running|Exited|Absent
    fn remove_container(&self, name: &str, force: bool) -> Result<()>;
    fn list_by_label(&self, label: &str) -> Result<Vec<String>>;
    fn host_uid(&self) -> u32;
}
```

- Real impl `PodmanCli` in `contracts/real.rs` shells out via
  `std::process::Command` (no async; matches the codebase).
- `FakeContainerRuntime` in `src/testing/` records calls + scripts responses, so
  the app-layer branching, teardown, and reattach logic is fully unit-testable
  without Podman (SPECS §27).
- Added to `Services` as `pub container: &'a dyn ContainerRuntime`. **Tests and
  the synchronous `cmd_new_agent_tab` path get a fake**; only `lib.rs` wires the
  real `PodmanCli`.

---

## 10. Auth

Resolved at spec-build time from `[execution.auth]`:

- **Mount mode:** `--volume <host_cred_path>:<container_path>:ro` (e.g.
  `~/.claude` → `/home/agent/.claude`). Guardrails forbid mounting `$HOME` root,
  so creds must be a subdirectory (they are).
- **Env mode:** for each name in `auth.env_allow`, read from the host process env
  and emit `--env NAME=<value>` as discrete argv. Never inherit full env
  (no `--env-host`, enforced).

Both can be combined. Default for the shipped wizard: mount `~/.claude`
read-only for the Claude agent (zero-setup reuse of the existing host login).

Status hooks keep working for free: the agent writes
`/workspace/.flightdeck/agent-status`, which is the bind-mounted worktree, so
`AppState::poll_status_files` reads it on the host unchanged (SPECS §24).

---

## 11. macOS / Podman specifics (must-verify)

FlightDeck is macOS-first; Podman on macOS runs a Linux VM, which changes two
things vs. DAC's Linux assumptions — **verify in the Phase 0 spike**:

- **Bind-mount must be inside a path shared into the Podman machine.** The
  default machine shares `$HOME`; worktrees live under the repo (under `$HOME`)
  so this normally holds. Add a `doctor` check that the worktree path is within a
  machine-shared directory; warn clearly otherwise.
- **Mount flags.** DAC uses `:z` (SELinux relabel). On the macOS virtiofs share
  `:z` may be unnecessary or rejected. The builder should select mount-flag
  suffix by platform (none on macOS, `:z` on Linux) — keep it a single function
  `mount_flags()` so it's easy to tune after the spike.
- **`--userns keep-id` + `--user <uid>`** behaviour across the VM boundary needs
  confirming for writable `/workspace`; this is DAC's fix for the UID-mismatch
  `Permission denied`.

A `flightdeck doctor` (or a preflight invoked before the first containerized
launch) verifies: podman on PATH, machine running, image present/current,
worktree path shareable. Surface failures as `Effect::Warning`/refusal — never a
silent broken launch.

---

## 12. Phasing (each phase independently shippable / reviewable)

**Phase 0 — Spike (no product code merged).** Manually prove on macOS:
`podman run -it` survives client kill; `podman attach` reconnects PTY with
resize + Ctrl-C; bind-mount + `keep-id` gives writable `/workspace`; settle mount
flags. Output: a short findings note that locks §7.1 and §11. *Gate for the
rest.*

**Phase 1 — Pure core (no runtime needed).** `runtime/spec.rs`,
`runtime/container.rs`, `runtime/guards.rs`, `runtime/name.rs`,
`runtime/image.rs` tag/hash logic. Full unit tests on argv + guardrails +
staleness hash. `ExecutionConfig` + validation + `TabState` fields. Nothing
wired into spawn yet. (Mirrors DAC's builder/guards/config test suites.)

**Phase 2 — `ContainerRuntime` + reattach plumbing.** Trait + `PodmanCli` +
`FakeContainerRuntime`; add to `Services`. Branch the three spawn sites; add
container teardown + startup reconcile. App-layer unit tests with the fake cover
primary-run, child-exec, reattach-vs-session-lost, and teardown. Behind
`[execution] enabled=false` so default behaviour is unchanged.

**Phase 3 — Image subsystem.** Shipped `containers/Containerfile.*`; generated
Containerfile templating; `flightdeck image build [--force]` on the background
worker; auto-build + preflight before first launch; `flightdeck doctor`.

**Phase 4 — End-to-end + polish.** Ignored-by-default Podman-backed e2e
(`#[ignore]`, like the real-PTY smoke test): build → run claude in a worktree →
edit a file → see it on host → child shell exec → restart-reattach → close +
cleanup. Wizard writes `[execution]`. Docs.

**Fast-follow (post-v1):** ad-hoc `flightdeck port <tab> <port>`; egress proxy
(reintroduces a minimal pod); Docker backend behind the same trait.

---

## 13. Open questions / risks

1. **Reattach (the big one).** Entirely contingent on the Phase 0 spike. If
   `attach` can't cleanly re-drive the PTY, persist-&-reattach degrades to
   "running container detected → offer restart" rather than seamless reattach.
2. **Resource limits default.** Confirm: limits **on** by default (4cpu/8g/512)
   vs. off-unless-configured. Plan assumes on with those values.
3. **`--rm` vs. explicit removal.** `--rm` is simplest and matches DAC, but if
   the spike shows attach is unreliable with `--rm`, switch to no-`--rm` +
   FlightDeck-owned `rm` on teardown. Localized to `build_run_args` + §7.3.
4. **Image customization default tier.** Plan leads with declarative
   `packages`/`setup_script`; `containerfile` is the advanced escape hatch.
5. **`flightdeck doctor`.** New CLI surface (currently CLI is minimal manual
   dispatch in `lib.rs`); small addition but worth confirming the command name.
6. **Status-hook path inside the container** assumes the agent honors
   `/workspace/.flightdeck/agent-status`; the bind mount makes it host-visible.
   Confirm the shipped base images set the hook up (or document it).

---

## 14. What deliberately does NOT change

- `terminal/` (PTY session, VT parser, selection, scrollback), rendering, input,
  the event loop, status classification, notifications.
- `GitExecutor` and the worktree model — worktrees stay host-side; git push /
  merge / rebase are unaffected (SPECS §5/§5.1 boundary intact).
- The recovery invariant: recovery never *relaunches*; reattach only reconnects
  to an already-*running* container (SPECS §10).
- Default behaviour with `[execution]` absent or `enabled=false` is bit-for-bit
  today's FlightDeck.
```
