// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
afterEach(() => { localStorage.clear(); vi.resetModules(); });
it.each([null, "{}", '{"theme":"dark"}', "invalid", "null", '{"claudeBypassPermissions":"false","codexBypassPermissions":0}'])("defaults permission bypass on for legacy or corrupt preferences: %s", async value => {
  localStorage.clear(); if (value !== null) localStorage.setItem("prefs", value);
  vi.resetModules();
  const { prefs } = await import("./prefs");
  expect(prefs.get()).toMatchObject({ claudeBypassPermissions: true, codexBypassPermissions: true });
});
it("keeps explicit false values after reloading preferences", async () => {
  localStorage.setItem("prefs", '{"claudeBypassPermissions":false,"codexBypassPermissions":true}');
  vi.resetModules();
  const { prefs } = await import("./prefs");
  expect(prefs.get()).toMatchObject({ claudeBypassPermissions: false, codexBypassPermissions: true });
  prefs.set(p => ({ ...p, codexBypassPermissions: false }));
  vi.resetModules();
  expect((await import("./prefs")).prefs.get()).toMatchObject({ claudeBypassPermissions: false, codexBypassPermissions: false });
});
it("keeps a default agent and drops a corrupt one", async () => {
  localStorage.setItem("prefs", '{"defaultAgent":"codex"}');
  vi.resetModules();
  expect((await import("./prefs")).prefs.get().defaultAgent).toBe("codex");
  localStorage.setItem("prefs", '{"defaultAgent":7}');
  vi.resetModules();
  expect((await import("./prefs")).prefs.get().defaultAgent).toBeNull();
});
