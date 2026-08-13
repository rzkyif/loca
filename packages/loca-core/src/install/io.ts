// Minimal stdio for the install/uninstall subcommands.
//
// These run the core binary as a short-lived CLI (not the booted server), so
// they deliberately bypass the file logger in shared/log.ts (which wants
// initLogger + a writable logs dir): human-readable progress goes to stderr,
// and the ONE machine-readable result the client parses - the pairing-code JSON
// after "set up a local Core", start-service's supervisor + pid - goes to clean
// stdout. Writing to Deno.std{out,err} directly (rather than console.*) keeps
// stdout free of any logger formatting so the JSON line is the only thing on it.

const enc = new TextEncoder();

/** A line of progress for a human watching the install; goes to stderr so it
 *  never contaminates the stdout the client parses. */
export function progress(msg: string): void {
  Deno.stderr.writeSync(enc.encode(msg + "\n"));
}

/** The single structured result of a subcommand, emitted as one JSON line on
 *  stdout for a machine caller to parse. */
export function emitJson(value: unknown): void {
  Deno.stdout.writeSync(enc.encode(JSON.stringify(value) + "\n"));
}
