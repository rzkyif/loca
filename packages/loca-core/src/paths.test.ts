import { assertEquals, assertThrows } from "@std/assert";
import { join } from "@std/path";
import {
  channelBinName,
  channelSuffix,
  clientRoot,
  corePort,
  coreRoot,
  llmPort,
  paths,
  speechPort,
} from "./paths.ts";

// paths.ts reads HOME / LOCA_CHANNEL / LOCA_CORE_HOME at call time. Snapshot
// and restore the relevant vars around each case so they don't leak into
// sibling tests (the suite runs single-threaded, so save/restore is enough).
function withEnv(env: Record<string, string | undefined>, fn: () => void): void {
  const keys = ["HOME", "USERPROFILE", "LOCA_CHANNEL", "LOCA_CORE_HOME", "LOCA_WORKERS_DIR"];
  const prior: Record<string, string | undefined> = {};
  for (const k of keys) prior[k] = Deno.env.get(k);
  try {
    for (const k of keys) {
      const v = env[k];
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
    fn();
  } finally {
    for (const k of keys) {
      const v = prior[k];
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
  }
}

const HOME = "/fake/home";

Deno.test("stable channel nests core/client under ~/.loca/stable", () => {
  withEnv({ HOME, LOCA_CHANNEL: undefined, LOCA_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".loca", "stable", "core"));
    assertEquals(clientRoot(), join(HOME, ".loca", "stable", "client"));
  });
});

Deno.test("dev and latest channels each get their own subtree", () => {
  withEnv({ HOME, LOCA_CHANNEL: "dev", LOCA_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".loca", "dev", "core"));
    assertEquals(clientRoot(), join(HOME, ".loca", "dev", "client"));
  });
  withEnv({ HOME, LOCA_CHANNEL: "latest", LOCA_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".loca", "latest", "core"));
    assertEquals(clientRoot(), join(HOME, ".loca", "latest", "client"));
  });
});

Deno.test("models dir is shared across channels at ~/.loca/models", () => {
  for (const ch of [undefined, "dev", "latest"]) {
    withEnv({ HOME, LOCA_CHANNEL: ch, LOCA_CORE_HOME: undefined }, () => {
      assertEquals(paths().modelsDir, join(HOME, ".loca", "models"));
    });
  }
});

Deno.test("LOCA_CORE_HOME override wins and keeps models inside it", () => {
  withEnv({ HOME, LOCA_CHANNEL: "dev", LOCA_CORE_HOME: "/tmp/iso" }, () => {
    assertEquals(coreRoot(), "/tmp/iso");
    assertEquals(paths().modelsDir, join("/tmp/iso", "models"));
  });
});

Deno.test("an unknown LOCA_CHANNEL throws rather than mis-isolating", () => {
  withEnv({ HOME, LOCA_CHANNEL: "nope", LOCA_CORE_HOME: undefined }, () => {
    assertThrows(() => coreRoot());
  });
});

Deno.test("channelSuffix is bare on stable and suffixed otherwise", () => {
  withEnv({ HOME, LOCA_CHANNEL: undefined }, () => {
    assertEquals(channelSuffix(), "");
  });
  withEnv({ HOME, LOCA_CHANNEL: "latest" }, () => assertEquals(channelSuffix(), "-latest"));
  withEnv({ HOME, LOCA_CHANNEL: "dev" }, () => assertEquals(channelSuffix(), "-dev"));
});

Deno.test("channelBinName suffixes Loca's own binaries per channel", () => {
  withEnv({ HOME, LOCA_CHANNEL: undefined }, () => {
    assertEquals(channelBinName("loca-core"), "loca-core");
  });
  withEnv({ HOME, LOCA_CHANNEL: "latest" }, () => {
    assertEquals(channelBinName("loca-core"), "loca-core-latest");
    assertEquals(channelBinName("loca-core-updater"), "loca-core-updater-latest");
  });
});

Deno.test("default ports are offset per channel so channels coexist", () => {
  withEnv({ HOME, LOCA_CHANNEL: undefined }, () => {
    assertEquals(corePort(), 7800);
    assertEquals(llmPort(), 7701);
    assertEquals(speechPort(), 7702);
  });
  withEnv({ HOME, LOCA_CHANNEL: "latest" }, () => {
    assertEquals(corePort(), 7810);
    assertEquals(llmPort(), 7711);
    assertEquals(speechPort(), 7712);
  });
  withEnv({ HOME, LOCA_CHANNEL: "dev" }, () => {
    assertEquals(corePort(), 7820);
    assertEquals(llmPort(), 7721);
    assertEquals(speechPort(), 7722);
  });
});
