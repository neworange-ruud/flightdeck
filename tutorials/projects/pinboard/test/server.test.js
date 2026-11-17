// The dev server serves the board and its assets, and nothing outside public/.
const { test, before, after } = require("node:test");
const assert = require("node:assert");
const { spawn } = require("node:child_process");
const path = require("node:path");

const PORT = 3999;
const BASE = `http://localhost:${PORT}`;
let server;

before(async () => {
  server = spawn(process.execPath, [path.join(__dirname, "..", "server.js")], {
    env: { ...process.env, PORT: String(PORT) },
  });
  await new Promise((resolve) => server.stdout.once("data", resolve));
});

after(() => server.kill());

test("serves the board", async () => {
  const res = await fetch(`${BASE}/`);
  assert.strictEqual(res.status, 200);
  assert.match(await res.text(), /Pinboard/);
});

test("serves the app script as JavaScript", async () => {
  const res = await fetch(`${BASE}/app.js`);
  assert.strictEqual(res.status, 200);
  assert.match(res.headers.get("content-type"), /javascript/);
});

test("answers 404 for a missing file", async () => {
  const res = await fetch(`${BASE}/nope.txt`);
  assert.strictEqual(res.status, 404);
});
