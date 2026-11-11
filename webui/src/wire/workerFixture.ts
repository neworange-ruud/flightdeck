/**
 * A FlightDeck Worker's snapshot, on the wire, for the tests
 * (`remote-control-vjtb`). Only the unit tests read it.
 *
 * Shaped like what `src/host/worker` sends a freshly attached window: v7,
 * `host_kind: "worker"`, the worker's own state on `worker`, and — unless a
 * test asks for some — no projects, which is what a fresh worker has.
 */

import type { WireProjectView, WireSnapshot, WireWorkerSnapshot } from "./frames";
import { PROTOCOL_VERSION } from "./frames";

export const WORKER_HOME = "/home/me";

export function workerWire(overrides: Partial<WireWorkerSnapshot> = {}): WireWorkerSnapshot {
  return {
    home: WORKER_HOME,
    shell: {
      terminal_id: "worker:shell",
      title: "Worker shell",
      geometry: { cols: 80, rows: 24 },
      byte_len: 0,
      replay_from: 0,
      alive: true,
      exit_code: null,
    },
    health: {
      sampled_at_ms: 1_000,
      uptime_secs: 60,
      version: "9.9.9",
      load_avg_1m_centi: 42,
      cpu_count: 8,
      mem_total_bytes: null,
      mem_available_bytes: null,
      disk_free_home_bytes: 20_000_000_000,
      disk_free_projects_root_bytes: 18_000_000_000,
      agents_total: 3,
      agents_running: 2,
    },
    status: {
      listening: ["100.64.0.1:7420"],
      advertised: "box.tail1234.ts.net:7420",
      not_listening_reason: null,
      unread_messages: 0,
      projects_root: `${WORKER_HOME}/flightdeck`,
    },
    connections: { window: null, background_links: 0 },
    host_messages: [],
    ...overrides,
  };
}

/** One project with one session and one agent terminal, as a worker lists it. */
export function workerProject(): WireProjectView {
  return {
    project_id: "p-alpha",
    name: "alpha",
    root: `${WORKER_HOME}/flightdeck/alpha`,
    base_branch: "main",
    dot: "idle",
    sessions: [
      {
        session_id: "s-1",
        project_id: "p-alpha",
        name: "fix-login",
        agent: "fake",
        agent_display_name: "Fake",
        phase: "ready",
        status: {
          interpreted: "idle",
          manual: null,
          bucket: "idle",
          running_time_secs: 0,
        },
        git: {
          branch: "fix-login",
          added: 0,
          modified: 0,
          removed: 0,
          ahead: 0,
          behind: 0,
          drift: 0,
          has_upstream: false,
          files_changed: 0,
          collected: true,
        },
        terminals: [
          {
            terminal_id: "t-agent",
            session_id: "s-1",
            role: "primary",
            title: "agent",
            geometry: { cols: 132, rows: 40 },
            byte_len: 0,
            replay_from: 0,
            alive: true,
          },
        ],
        lifecycle_reporting: true,
      },
    ],
  };
}

export function workerSnapshotWire(
  overrides: Partial<WireSnapshot> = {},
): WireSnapshot {
  return {
    type: "snapshot",
    protocol_version: PROTOCOL_VERSION,
    host_version: "9.9.9",
    server_time_ms: 2_000,
    viewer_id: "v1",
    seat: "writing",
    seats: [],
    last_input_seq: 0,
    projects: [],
    selection: {},
    geometry: { cols: 80, rows: 24 },
    replay_capacity_bytes: 1024,
    activity: [],
    commands: [],
    host_kind: "worker",
    worker: workerWire(),
    ...overrides,
  };
}
