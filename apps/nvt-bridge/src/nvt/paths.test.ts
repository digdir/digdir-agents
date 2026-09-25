import assert from "node:assert/strict";
import { test } from "node:test";
import {
  agentCanTraverse,
  agentCanWrite,
  ancestorPaths,
  assertAgentCanTraverse,
  traversalProblems,
  type StatFn,
} from "./paths.ts";

/** Kataloger med mode/eier, som en liten host. Ukjente stier «finnes ikke». */
function fakeStat(tree: Record<string, { mode: number; uid?: number; gid?: number }>): StatFn {
  return async (target) => {
    const entry = tree[target];
    if (!entry) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
    return { mode: entry.mode, uid: entry.uid ?? 0, gid: entry.gid ?? 0 };
  };
}

test("ancestorPaths gir alle ledd, rot først", () => {
  assert.deepEqual(ancestorPaths("/srv/nvt-agent"), ["/", "/srv", "/srv/nvt-agent"]);
  assert.deepEqual(ancestorPaths("/"), ["/"]);
  assert.deepEqual(ancestorPaths("/a/b/../c"), ["/", "/a", "/a/c"]);
});

test("agentCanTraverse: o+x, eller eier/gruppe 1000 med x", () => {
  assert.equal(agentCanTraverse({ mode: 0o755, uid: 0, gid: 0 }), true);
  assert.equal(agentCanTraverse({ mode: 0o700, uid: 0, gid: 0 }), false, "/root");
  assert.equal(agentCanTraverse({ mode: 0o700, uid: 1000, gid: 1000 }), true, "egen katalog");
  assert.equal(agentCanTraverse({ mode: 0o710, uid: 0, gid: 1000 }), true, "gruppe 1000");
  assert.equal(agentCanTraverse({ mode: 0o644, uid: 1000, gid: 1000 }), false, "ingen x");
});

test("traversalProblems peker på det leddet som stopper uid 1000", async () => {
  const stat = fakeStat({
    "/": { mode: 0o755 },
    "/root": { mode: 0o700 },
    "/root/src": { mode: 0o755, uid: 0 },
  });
  const problems = await traversalProblems("/root/src", stat);
  assert.deepEqual(
    problems.map((p) => p.path),
    ["/root"],
  );
  assert.equal(problems[0]!.mode, 0o700);
});

test("traversalProblems er tom for en sti alle kommer gjennom", async () => {
  const stat = fakeStat({
    "/": { mode: 0o755 },
    "/srv": { mode: 0o755 },
    "/srv/nvt-agent": { mode: 0o750, uid: 1000, gid: 1000 },
  });
  assert.deepEqual(await traversalProblems("/srv/nvt-agent", stat), []);
});

test("en manglende katalog rapporteres, og resten av stien droppes", async () => {
  const stat = fakeStat({ "/": { mode: 0o755 } });
  const problems = await traversalProblems("/srv/nvt-agent", stat);
  assert.deepEqual(problems, [{ path: "/srv", reason: "missing" }]);
});

test("agentCanWrite: w for other, eller eier/gruppe 1000 med w", () => {
  assert.equal(agentCanWrite({ mode: 0o777, uid: 0, gid: 0 }), true);
  assert.equal(agentCanWrite({ mode: 0o755, uid: 0, gid: 0 }), false, "root-eid triggers/");
  assert.equal(agentCanWrite({ mode: 0o755, uid: 1000, gid: 1000 }), true);
  assert.equal(agentCanWrite({ mode: 0o775, uid: 0, gid: 1000 }), true);
});

test("skrivekrav gjelder katalogen selv, ikke foreldrene", async () => {
  const stat = fakeStat({
    "/": { mode: 0o755 },
    "/srv": { mode: 0o755 },
    "/srv/triggers": { mode: 0o755, uid: 0, gid: 0 },
  });
  assert.deepEqual(await traversalProblems("/srv/triggers", stat), [], "traversering er ok");
  const problems = await traversalProblems("/srv/triggers", stat, { requireWrite: true });
  assert.deepEqual(problems, [
    { path: "/srv/triggers", reason: "not-writable", mode: 0o755, uid: 0 },
  ]);
});

test("assertAgentCanTraverse kaster med årsak, sti og /root-hintet", async () => {
  const stat = fakeStat({ "/": { mode: 0o755 }, "/root": { mode: 0o700 }, "/root/nvt": { mode: 0o755 } });
  await assert.rejects(
    () =>
      assertAgentCanTraverse("NVT_ROOT", "/root/nvt", {
        statFn: stat,
        platform: "linux",
        containerized: false,
      }),
    (err: Error) => {
      assert.match(err.message, /NVT_ROOT="\/root\/nvt"/);
      assert.match(err.message, /\/root \(mode 0700, eier uid 0\)/);
      assert.match(err.message, /uid 1000/);
      assert.match(err.message, /\/srv\/nvt-agent/, "hintet om en fungerende sti");
      return true;
    },
  );
});

test("relative stier avvises — bind-mounts kan ikke oversettes", async () => {
  await assert.rejects(
    () => assertAgentCanTraverse("NVT_ROOT", "../nvt-agent", { platform: "linux" }),
    /absolutt sti/,
  );
});

test("symlenker løses før vandringen (ellers skjules målets foreldre)", async () => {
  // /opt/link → /root/hidden. Leksikalsk vandring ville aldri sett /root.
  const stat = fakeStat({
    "/": { mode: 0o755 },
    "/opt": { mode: 0o755 },
    "/opt/link": { mode: 0o777 },
    "/root": { mode: 0o700 },
    "/root/hidden": { mode: 0o755 },
  });
  await assert.rejects(
    () =>
      assertAgentCanTraverse("NVT_ROOT", "/opt/link", {
        statFn: stat,
        realpathFn: async (p) => (p === "/opt/link" ? "/root/hidden" : p),
        platform: "linux",
        containerized: false,
      }),
    (err: Error) => {
      assert.match(err.message, /realpath: "\/root\/hidden"/);
      assert.match(err.message, /\/root \(mode 0700/);
      return true;
    },
  );
});

test("i container er funnet en advarsel — vi ser ikke hostens mode-bits", async () => {
  const stat = fakeStat({ "/": { mode: 0o755 }, "/root": { mode: 0o700 }, "/root/nvt": { mode: 0o755 } });
  const logged: string[] = [];
  await assertAgentCanTraverse("NVT_ROOT", "/root/nvt", {
    statFn: stat,
    platform: "linux",
    containerized: true,
    log: (m) => logged.push(m),
  });
  assert.match(logged[0]!, /ADVARSEL.*kjører i container og ser ikke hostens mode-bits/s);
});

test("utenfor Linux er funnet en advarsel, ikke en blokkering", async () => {
  const stat = fakeStat({ "/": { mode: 0o755 }, "/Users": { mode: 0o700 }, "/Users/nvt": { mode: 0o755 } });
  const logged: string[] = [];
  await assertAgentCanTraverse("NVT_ROOT", "/Users/nvt", {
    statFn: stat,
    platform: "darwin",
    log: (m) => logged.push(m),
  });
  assert.equal(logged.length, 1);
  assert.match(logged[0]!, /ADVARSEL.*ikke håndhevet på darwin/s);
});

test("rømningsluka hopper over sjekken og sier det", async () => {
  const logged: string[] = [];
  await assertAgentCanTraverse("NVT_ROOT", "/root/nvt", {
    skip: true,
    platform: "linux",
    log: (m) => logged.push(m),
    statFn: async () => {
      throw new Error("skal ikke kalles");
    },
  });
  assert.match(logged[0]!, /hoppet over/);
});
