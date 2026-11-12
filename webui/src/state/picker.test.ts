import { describe, expect, it } from "vitest";
import {
  browseRequest,
  CLONE_URL_MISSING,
  cloneLine,
  cloneNotice,
  cloneRequest,
  newPicker,
  openRequest,
  pickerAck,
  pickerBusy,
  pickerCloneProgress,
  pickerCloneResult,
  pickerListing,
  pickerScan,
  pickerSent,
  scanRequest,
  tildePath,
} from "./picker";
import type { PickerListing, PickerState } from "./picker";

/**
 * The worker's project picker (`remote-control-vjtb`), rule for rule with the
 * native `ProjectPicker` (`src/web/client/worker.rs`): what each click sends,
 * that a superseded answer is dropped, and that every refusal is the worker's
 * own words.
 */

const HOME = "/home/me";

function listing(seq: number, path: string): PickerListing {
  return {
    seq,
    path,
    parent: path === HOME ? null : HOME,
    home: HOME,
    entries: [{ name: "code", path: `${HOME}/code`, isGitRepo: false, isOpenProject: false }],
    files: [],
    truncated: false,
  };
}

describe("what a click sends", () => {
  it("browses $HOME with no path, and honours the hidden toggle", () => {
    const picker = newPicker(HOME);
    expect(browseRequest(picker, null)).toEqual({
      name: "browse_dir",
      args: { show_hidden: false },
      sent: { kind: "browse" },
    });
    expect(browseRequest({ ...picker, showHidden: true }, "  ~/src ")).toEqual({
      name: "browse_dir",
      args: { path: "~/src", show_hidden: true },
      sent: { kind: "browse" },
    });
    expect(browseRequest(picker, "   ").args).toEqual({ show_hidden: false });
  });

  it("scans with no args, and opens by path", () => {
    expect(scanRequest()).toEqual({ name: "scan_projects", sent: { kind: "scan" } });
    expect(openRequest("/home/me/x")).toEqual({
      name: "open_project_path",
      args: { path: "/home/me/x" },
      sent: { kind: "open", path: "/home/me/x" },
    });
  });

  it("clones the URL field, with the name only when one is typed", () => {
    const picker = { ...newPicker(HOME), urlField: " git@x:me/r.git ", nameField: "" };
    expect(cloneRequest(picker)).toEqual({
      name: "clone_project",
      args: { url: "git@x:me/r.git" },
      sent: { kind: "clone", url: "git@x:me/r.git" },
    });
    expect(cloneRequest({ ...picker, nameField: " mine " })).toMatchObject({
      args: { url: "git@x:me/r.git", name: "mine" },
    });
    expect(cloneRequest({ ...picker, urlField: "  " })).toEqual({ error: CLONE_URL_MISSING });
  });
});

describe("answers", () => {
  it("takes the latest browse's listing and moves the path bar to it", () => {
    let picker = pickerSent(newPicker(HOME), { kind: "browse" }, 4);
    expect(pickerBusy(picker)).toBe(true);
    picker = pickerListing(picker, listing(4, HOME));
    expect(picker.cwd).toBe(HOME);
    expect(picker.parent).toBeNull();
    expect(picker.pathField).toBe(HOME);
    expect(picker.entries.map((e) => e.name)).toEqual(["code"]);
    expect(pickerBusy(picker)).toBe(false);
  });

  it("drops a listing for a browse that has been superseded", () => {
    let picker = pickerSent(newPicker(HOME), { kind: "browse" }, 4);
    picker = pickerSent(picker, { kind: "browse" }, 5);
    expect(pickerListing(picker, listing(4, `${HOME}/old`))).toBe(picker);
    expect(pickerListing(picker, listing(5, `${HOME}/code`)).cwd).toBe(`${HOME}/code`);
  });

  it("puts a refused scan on the error line, and keeps what it found", () => {
    const picker = pickerScan(pickerSent(newPicker(HOME), { kind: "scan" }, 7), {
      seq: 7,
      root: "/elsewhere",
      rootExists: false,
      repos: [],
      refusal: "The projects root is outside $HOME.",
    });
    expect(picker.error).toBe("The projects root is outside $HOME.");
    expect(picker.scanned).toEqual({ root: "/elsewhere", rootExists: false, repos: [] });
  });

  it("a new request clears the last error and status", () => {
    const failed: PickerState = { ...newPicker(HOME), error: "no", status: "yes" };
    const sent = pickerSent(failed, { kind: "scan" }, 9);
    expect(sent.error).toBeNull();
    expect(sent.status).toBeNull();
  });
});

describe("acks", () => {
  it("a refused browse is the error line, in the worker's words", () => {
    const picker = pickerSent(newPicker(HOME), { kind: "browse" }, 3);
    const answered = pickerAck(picker, 3, "rejected", "/etc is outside $HOME.");
    expect(answered.owned).toBe(true);
    expect(answered.picker.error).toBe("/etc is outside $HOME.");
    expect(pickerBusy(answered.picker)).toBe(false);
  });

  it("an applied browse changes nothing — the listing is the answer", () => {
    const picker = pickerSent(newPicker(HOME), { kind: "browse" }, 3);
    expect(pickerAck(picker, 3, "applied", null)).toEqual({ owned: true, picker });
  });

  it("an applied open is the project opened, with the worker's words as status", () => {
    const picker = pickerSent(newPicker(HOME), { kind: "open", path: `${HOME}/a` }, 11);
    const answered = pickerAck(picker, 11, "applied", "Opened project 'a'.");
    expect(answered.picker.opened).toEqual({ path: `${HOME}/a`, projectId: null });
    expect(answered.picker.status).toBe("Opened project 'a'.");
  });

  it("a refused open says why and opens nothing", () => {
    const picker = pickerSent(newPicker(HOME), { kind: "open", path: "/x" }, 11);
    const answered = pickerAck(picker, 11, "rejected", null);
    expect(answered.picker.opened).toBeNull();
    expect(answered.picker.error).toBe("The worker refused that.");
  });

  it("recognises a superseded request's ack as its own, and nobody else's", () => {
    let picker = pickerSent(newPicker(HOME), { kind: "browse" }, 1);
    picker = pickerSent(picker, { kind: "browse" }, 2);
    expect(pickerAck(picker, 1, "applied", null).owned).toBe(true);
    expect(pickerAck(picker, 99, "applied", null).owned).toBe(false);
  });
});

describe("clones", () => {
  const started = pickerSent(newPicker(HOME), { kind: "clone", url: "u" }, 20);

  it("follows its own clone's progress and nobody else's", () => {
    expect(cloneLine(started.clone)).toBe("Cloning u — Starting");
    const progressed = pickerCloneProgress(started, {
      cloneId: 20,
      url: "u",
      phase: "Receiving objects",
      percent: 42,
    });
    expect(cloneLine(progressed.clone)).toBe("Cloning u — Receiving objects 42%");
    expect(
      pickerCloneProgress(started, { cloneId: 21, url: "v", phase: "x", percent: 1 }),
    ).toBe(started);
  });

  it("a successful clone is a project opened, by id", () => {
    const done = pickerCloneResult(started, {
      cloneId: 20,
      url: "u",
      ok: true,
      path: `${HOME}/flightdeck/r`,
      projectId: "p-r",
      error: null,
    });
    expect(done.opened).toEqual({ path: `${HOME}/flightdeck/r`, projectId: "p-r" });
    expect(cloneLine(done.clone)).toBe(`Cloned into ${HOME}/flightdeck/r.`);
  });

  it("a failed clone is git's words on the error line", () => {
    const failed = pickerCloneResult(started, {
      cloneId: 20,
      url: "u",
      ok: false,
      path: null,
      projectId: null,
      error: "fatal: repository not found",
    });
    expect(failed.error).toBe("fatal: repository not found");
    expect(failed.opened).toBeNull();
    expect(cloneLine(failed.clone)).toBe("fatal: repository not found");
  });

  it("a clone the closed picker started ends as a notice", () => {
    const result = { cloneId: 1, url: "u", ok: true, path: "/p", projectId: null, error: null };
    expect(cloneNotice(result)).toBe("Cloned u into /p.");
    expect(cloneNotice({ ...result, path: null })).toBe("Cloned u.");
    expect(cloneNotice({ ...result, ok: false })).toBe("The clone failed.");
  });
});

describe("tildePath", () => {
  it("shows $HOME as ~, and nothing outside it differently", () => {
    expect(tildePath(HOME, HOME)).toBe("~");
    expect(tildePath(`${HOME}/flightdeck/a`, HOME)).toBe("~/flightdeck/a");
    expect(tildePath("/home/meadow/x", HOME)).toBe("/home/meadow/x");
    expect(tildePath("/srv/x", "")).toBe("/srv/x");
  });
});
