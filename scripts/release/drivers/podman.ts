// Linux build environment: a Podman container, started on demand.
//
// Builds core + helpers + speech (buildCore) and the Tauri Linux desktop client
// AppImage (buildClient) for x86_64-unknown-linux-gnu by CROSS-COMPILING from the
// NATIVE arm64 container (qemu-emulated rustc SIGSEGVs, so no amd64 emulation):
// rustc runs arm64 + emits x64; the GNU cross toolchain compiles/links the C deps;
// the client links the GUI libs against an amd64 sysroot baked into the image, and
// linuxdeploy bundles that .so closure into the AppImage. See the Containerfile for
// the toolchain + sysroot setup and the magic-patched AppImage tooling.
//
// Device-specific config comes from .env (LOCA_PODMAN_* / LOCA_LINUX_BUILD_*,
// promoted into the process env by loadDriverEnv; see .env.example). Read lazily
// so .env is loaded first. Until LOCA_PODMAN_MACHINE is set, available() returns
// false and the driver is skipped.
function cfg() {
  return {
    // `podman machine list` -> the NAME column.
    machine: Deno.env.get("LOCA_PODMAN_MACHINE") ?? "",
    // Image tag built from drivers/linux/Containerfile (see its header for the
    // `podman build` command). Must contain deno + the Rust linux targets + the
    // Tauri Linux deps + sherpa build deps.
    image: Deno.env.get("LOCA_LINUX_BUILD_IMAGE") ?? "loca-linux-build:latest",
  };
}

import { join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import type { Triple } from "../../../packages/loca-shared/src/domain/model.ts";
import {
  type ArtifactBundle,
  type ClientDescriptor,
  readBundle,
  readClientDescriptor,
} from "../artifacts.ts";
import { DIST_DIR, ok, REPO_ROOT, step } from "../lib.ts";
import type { BuildEnvironment, ClientBuildRequest, CoreBuildRequest, EnvState } from "./mod.ts";

// Only x86_64 is in the release matrix (linux-arm64 is dropped as niche for a
// desktop GUI). The container runs NATIVE arm64 and cross-compiles to x64 (see
// the Containerfile) - no emulation, so no qemu rustc crashes.
const LINUX_TRIPLES: Triple[] = ["x86_64-unknown-linux-gnu"];

async function podman(args: string[]): Promise<{ code: number; stdout: string }> {
  const out = await new Deno.Command("podman", {
    args,
    stdout: "piped",
    stderr: "inherit",
  }).output();
  return {
    code: out.code,
    stdout: new TextDecoder().decode(out.stdout).trim(),
  };
}

async function hasPodman(): Promise<boolean> {
  try {
    return (
      await new Deno.Command("podman", {
        args: ["--version"],
        stdout: "null",
        stderr: "null",
      }).output()
    ).success;
  } catch {
    return false;
  }
}

export const podmanLinuxDriver: BuildEnvironment = {
  id: "linux-podman",
  triples: LINUX_TRIPLES,

  async available(): Promise<boolean> {
    return cfg().machine !== "" && (await hasPodman());
  },

  async detectState(): Promise<EnvState> {
    const { code, stdout } = await podman([
      "machine",
      "inspect",
      cfg().machine,
      "--format",
      "{{.State}}",
    ]);
    if (code !== 0) return "ABSENT";
    return stdout.toLowerCase() === "running" ? "RUNNING" : "STOPPED";
  },

  async ensureUp(): Promise<void> {
    const machine = cfg().machine;
    const { code } = await podman(["machine", "start", machine]);
    if (code !== 0) throw new Error(`podman machine start ${machine} failed`);
  },

  async teardown(): Promise<void> {
    await podman(["machine", "stop", cfg().machine]);
  },

  teardownSync(): void {
    try {
      new Deno.Command("podman", {
        args: ["machine", "stop", cfg().machine],
        stdout: "null",
        stderr: "null",
      }).outputSync();
    } catch {
      /* best-effort */
    }
  },

  async buildCore(req: CoreBuildRequest): Promise<ArtifactBundle> {
    const records: ArtifactBundle["records"] = [];
    let version = "";
    // The rw bind-mount target must exist on the host (else podman creates it
    // root-owned, or the mount fails); the build writes artifacts + bundle here.
    await ensureDir(DIST_DIR);
    for (const triple of req.triples) {
      step(`podman build core for ${triple}`);
      // Per-triple staging dir under dist/ (a nested rw mount over the ro repo),
      // so artifacts + bundle.json land directly on the host with no podman cp.
      const stageRel = `bundles/${this.id}-${triple}`;
      const { code } = await podman([
        "run",
        "--rm",
        "-v",
        `${REPO_ROOT}:/work:ro`,
        // Writable dist overlaid on the read-only repo: builds write here, the
        // host reads here. cargo's target dir is sent to /tmp (writable, fast).
        "-v",
        `${DIST_DIR}:/work/dist:rw`,
        "-e",
        "CARGO_TARGET_DIR=/tmp/target",
        "-w",
        "/work",
        cfg().image,
        "deno",
        "run",
        "-A",
        "scripts/build-release-bundle.ts",
        "--kind=core",
        `--channel=${req.channel}`,
        `--target=${triple}`,
        `--bundle-dir=/work/dist/${stageRel}`,
      ]);
      if (code !== 0) throw new Error(`podman core build for ${triple} exited ${code}`);
      const bundle = await readBundle(join(DIST_DIR, stageRel));
      version = bundle.version;
      records.push(...bundle.records);
      ok(`${triple}: ${bundle.records.length} artifacts`);
    }
    return { version, channel: req.channel, triples: req.triples, records };
  },

  async buildClient(req: ClientBuildRequest): Promise<ClientDescriptor[]> {
    await ensureDir(DIST_DIR);
    const descriptors: ClientDescriptor[] = [];
    for (const triple of req.triples) {
      step(`podman build client (AppImage) for ${triple}`);
      const stageRel = `bundles/${this.id}-client-${triple}`;
      const { code } = await podman([
        "run",
        "--rm",
        // Unlike the core build, the Tauri/vite frontend build writes into the
        // repo tree (build/, .svelte-kit/, node_modules/), so the client build
        // needs a WRITABLE repo mount. cargo + the Tauri bundler write to
        // /tmp/target (findClientBundle honors CARGO_TARGET_DIR); the AppImage
        // lands on the host via the writable mount under dist/.
        "-v",
        `${REPO_ROOT}:/work:rw`,
        "-e",
        "CARGO_TARGET_DIR=/tmp/target",
        // Cross-link the GUI app against the amd64 sysroot baked into the image
        // (webkit2gtk/gtk/soup/...). Overrides the core build's multiarch-dbus
        // pkg-config + plain cross gcc with sysroot-scoped equivalents, so the
        // x86_64 Tauri client resolves the webkit headers/libs the multiarch -dev
        // route can't provide. Client-only: the core build keeps the image env.
        "-e",
        "PKG_CONFIG_SYSROOT_DIR_x86_64_unknown_linux_gnu=/opt/amd64-sysroot",
        "-e",
        "PKG_CONFIG_PATH_x86_64_unknown_linux_gnu=/opt/amd64-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig:/opt/amd64-sysroot/usr/share/pkgconfig",
        // Plain PKG_CONFIG_PATH (no triple suffix) for linuxdeploy-plugin-gtk's own
        // pkg-config calls (librsvg-2.0 etc.); the Rust build uses the suffixed one
        // above, so this only steers the AppImage GTK-resource bundling.
        "-e",
        "PKG_CONFIG_PATH=/opt/amd64-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig:/opt/amd64-sysroot/usr/share/pkgconfig",
        "-e",
        "CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc --sysroot=/opt/amd64-sysroot",
        "-e",
        "CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ --sysroot=/opt/amd64-sysroot",
        "-e",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-C link-arg=--sysroot=/opt/amd64-sysroot",
        // linuxdeploy resolves the GUI .so closure to bundle INTO the AppImage by
        // DT_NEEDED + LD_LIBRARY_PATH; point it at the amd64 sysroot so it copies
        // the x86_64 webkit/gtk/... libs (not the host's arm64 ones).
        "-e",
        "LD_LIBRARY_PATH=/opt/amd64-sysroot/usr/lib/x86_64-linux-gnu:/opt/amd64-sysroot/lib/x86_64-linux-gnu:/opt/amd64-sysroot/usr/lib",
        // Tauri's AppImage bundler downloads linuxdeploy/appimagetool to
        // dirs::cache_dir()/tauri and runs them; the image bakes magic-patched
        // copies there (so they exec under qemu - see the Containerfile), found via
        // this XDG_CACHE_HOME override.
        "-e",
        "XDG_CACHE_HOME=/opt/tauri-cache",
        // Skip linuxdeploy's strip pass: its bundled x86_64 `strip`, run under
        // qemu, fails to recognize the (valid) x86_64 .so it just bundled ("Unable
        // to recognise the format"). Stripping only trims size, so disabling it
        // yields the same working AppImage. linuxdeploy reads NO_STRIP from the env
        // Tauri inherits.
        "-e",
        "NO_STRIP=1",
        // Make the image's `uname` shim report x86_64 so linuxdeploy-plugin-gtk
        // bundles the x86_64 GTK runtime (surfaced at the standard multiarch path in
        // the image), not the container's arm64 one. Scoped to the client build, so
        // the core build still sees the real host arch. See the Containerfile.
        "-e",
        "LOCA_FAKE_X86=1",
        // The Linux AppImage carries a Tauri updater .sig like every other desktop
        // installer, so the minisign private key transits here (the Ed25519
        // manifest key + R2 creds never leave the host). Empty password -> "".
        // envFromProcess (build-release-bundle.ts -> lib.ts) reads the TAURI_UPDATER_*
        // names from the container env; buildClient re-derives TAURI_SIGNING_PRIVATE_KEY
        // from them for the bundler. Matches the Windows driver + .env.
        "-e",
        `LOCA_SIGNING_PUBLIC_KEY_B64=${req.secrets.signingPublicKeyB64}`,
        "-e",
        `TAURI_UPDATER_PUBLIC_KEY=${req.secrets.tauriPublicKey}`,
        "-e",
        `TAURI_UPDATER_PRIVATE_KEY=${req.secrets.tauriPrivateKey}`,
        "-e",
        `TAURI_UPDATER_PRIVATE_KEY_PASSWORD=${req.secrets.tauriPassword}`,
        "-w",
        "/work",
        cfg().image,
        "deno",
        "run",
        "-A",
        "scripts/build-release-bundle.ts",
        "--kind=client",
        `--channel=${req.channel}`,
        `--target=${triple}`,
        // The AppImage is the sole Linux client format (self-contained + the
        // Tauri updater artifact); the client ships no deb/rpm. (The container
        // still carries dpkg-dev + rpm for the Core deb/rpm in core-installers.ts.)
        "--bundles=appimage",
        `--bundle-dir=/work/dist/${stageRel}`,
      ]);
      if (code !== 0) throw new Error(`podman client build for ${triple} exited ${code}`);
      const descriptor = await readClientDescriptor(join(DIST_DIR, stageRel));
      descriptors.push(descriptor);
      ok(`${triple}: client ${descriptor.filename}`);
    }
    return descriptors;
  },
};
