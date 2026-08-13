// Prepend a directory to the platform-specific shared-library search path.
// Returns the env additions and (on Windows) the cwd override the supervisor
// should apply when spawning the child.
//
// macOS:   DYLD_LIBRARY_PATH=<dir>:$DYLD_LIBRARY_PATH
// Linux:   LD_LIBRARY_PATH=<dir>:$LD_LIBRARY_PATH
// Windows: PATH=<dir>;$PATH + cwd=<dir>
//
// The Windows cwd override is load-bearing: ggml_backend_load_all() scans
// fs::current_path() for ggml-*.dll plugins. Without it, llama/whisper would
// fail to find their compute backends when spawned by a parent process.

export interface LibraryEnv {
  env: Record<string, string>;
  cwd?: string;
}

export function libraryEnvFor(
  dir: string,
  platform: typeof Deno.build.os = Deno.build.os,
): LibraryEnv {
  if (platform === "darwin") {
    return {
      env: {
        DYLD_LIBRARY_PATH: prepend(Deno.env.get("DYLD_LIBRARY_PATH"), dir, ":"),
      },
    };
  }
  if (platform === "linux") {
    return {
      env: {
        LD_LIBRARY_PATH: prepend(Deno.env.get("LD_LIBRARY_PATH"), dir, ":"),
      },
    };
  }
  if (platform === "windows") {
    return {
      env: { PATH: prepend(Deno.env.get("PATH"), dir, ";") },
      cwd: dir,
    };
  }
  return { env: {} };
}

function prepend(existing: string | undefined, addition: string, sep: string): string {
  if (!existing) return addition;
  return `${addition}${sep}${existing}`;
}
