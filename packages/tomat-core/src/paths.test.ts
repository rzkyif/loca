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

// paths.ts reads HOME / TOMAT_CHANNEL / TOMAT_CORE_HOME at call time. Snapshot
// and restore the relevant vars around each case so they don't leak into
// sibling tests (the suite runs single-threaded, so save/restore is enough).
function withEnv(env: Record<string, string | undefined>, fn: () => void): void {
  const keys = ["HOME", "USERPROFILE", "TOMAT_CHANNEL", "TOMAT_CORE_HOME", "TOMAT_WORKERS_DIR"];
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

Deno.test("stable channel nests core/client under ~/.tomat/stable", () => {
  withEnv({ HOME, TOMAT_CHANNEL: undefined, TOMAT_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".tomat", "stable", "core"));
    assertEquals(clientRoot(), join(HOME, ".tomat", "stable", "client"));
  });
});

Deno.test("dev and latest channels each get their own subtree", () => {
  withEnv({ HOME, TOMAT_CHANNEL: "dev", TOMAT_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".tomat", "dev", "core"));
    assertEquals(clientRoot(), join(HOME, ".tomat", "dev", "client"));
  });
  withEnv({ HOME, TOMAT_CHANNEL: "latest", TOMAT_CORE_HOME: undefined }, () => {
    assertEquals(coreRoot(), join(HOME, ".tomat", "latest", "core"));
    assertEquals(clientRoot(), join(HOME, ".tomat", "latest", "client"));
  });
});

Deno.test("models dir is shared across channels at ~/.tomat/models", () => {
  for (const ch of [undefined, "dev", "latest"]) {
    withEnv({ HOME, TOMAT_CHANNEL: ch, TOMAT_CORE_HOME: undefined }, () => {
      assertEquals(paths().modelsDir, join(HOME, ".tomat", "models"));
    });
  }
});

Deno.test("TOMAT_CORE_HOME override wins and keeps models inside it", () => {
  withEnv({ HOME, TOMAT_CHANNEL: "dev", TOMAT_CORE_HOME: "/tmp/iso" }, () => {
    assertEquals(coreRoot(), "/tmp/iso");
    assertEquals(paths().modelsDir, join("/tmp/iso", "models"));
  });
});

Deno.test("an unknown TOMAT_CHANNEL throws rather than mis-isolating", () => {
  withEnv({ HOME, TOMAT_CHANNEL: "nope", TOMAT_CORE_HOME: undefined }, () => {
    assertThrows(() => coreRoot());
  });
});

Deno.test("channelSuffix is bare on stable and suffixed otherwise", () => {
  withEnv({ HOME, TOMAT_CHANNEL: undefined }, () => {
    assertEquals(channelSuffix(), "");
  });
  withEnv({ HOME, TOMAT_CHANNEL: "latest" }, () => assertEquals(channelSuffix(), "-latest"));
  withEnv({ HOME, TOMAT_CHANNEL: "dev" }, () => assertEquals(channelSuffix(), "-dev"));
});

Deno.test("channelBinName suffixes Tomat's own binaries per channel", () => {
  withEnv({ HOME, TOMAT_CHANNEL: undefined }, () => {
    assertEquals(channelBinName("tomat-core"), "tomat-core");
  });
  withEnv({ HOME, TOMAT_CHANNEL: "latest" }, () => {
    assertEquals(channelBinName("tomat-core"), "tomat-core-latest");
    assertEquals(channelBinName("tomat-core-updater"), "tomat-core-updater-latest");
  });
});

Deno.test("default ports are offset per channel so channels coexist", () => {
  withEnv({ HOME, TOMAT_CHANNEL: undefined }, () => {
    assertEquals(corePort(), 7800);
    assertEquals(llmPort(), 7701);
    assertEquals(speechPort(), 7702);
  });
  withEnv({ HOME, TOMAT_CHANNEL: "latest" }, () => {
    assertEquals(corePort(), 7810);
    assertEquals(llmPort(), 7711);
    assertEquals(speechPort(), 7712);
  });
  withEnv({ HOME, TOMAT_CHANNEL: "dev" }, () => {
    assertEquals(corePort(), 7820);
    assertEquals(llmPort(), 7721);
    assertEquals(speechPort(), 7722);
  });
});
