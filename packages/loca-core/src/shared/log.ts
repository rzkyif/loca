// Structured logger wrapping @std/log. Writes to stderr (for the dev console
// + journal/launchd capture) and to ~/.loca/core/logs/core.log (rotated).
//
// All modules import getLogger("scope") to get a child logger; the scope
// becomes a prefix on every line.
//
// Every formatted line runs through `scrubSecrets` so credentials cannot
// leak into core.log or the dev console even if a caller accidentally
// interpolates one (e.g. a bad-auth error that includes the supplied
// token). Scrubbing is applied at format time, after `rec.msg` is built.

import * as log from "@std/log";
import { scrubSecrets } from "@loca/core-engine";
import { paths } from "../paths.ts";

let initialized = false;

// File handler: full ISO timestamp, no color, scrubbed. The on-disk core.log is
// a machine record, so its shape never changes.
function fileFormatter(rec: log.LogRecord): string {
  const ts = new Date(rec.datetime).toISOString();
  const scope = rec.loggerName === "default" ? "" : ` [${rec.loggerName}]`;
  return scrubSecrets(`${ts} ${rec.levelName}${scope} ${rec.msg}`);
}

// SGR color per level for the console formatter: DEBUG dim, INFO green, WARN
// yellow, ERROR red. Unknown levels fall through uncolored.
const LEVEL_COLOR: Record<string, string> = {
  DEBUG: "2",
  INFO: "32",
  WARN: "33",
  ERROR: "31",
};

// Console formatter, tuned for human reading.
//  - LOCA_LOG_NO_TIME=1 drops the timestamp (set by scripts/dev.ts, which owns
//    a single timestamp column for the multiplexed dev console). Otherwise a
//    compact HH:MM:SS.mmm wall-clock time is shown.
//  - Color is on when LOCA_LOG_COLOR=1 (forced by dev.ts, since core's stderr
//    is piped there and not a TTY) or when stderr is a TTY and NO_COLOR is unset.
const noTime = Deno.env.get("LOCA_LOG_NO_TIME") === "1";
const consoleColor =
  Deno.env.get("LOCA_LOG_COLOR") === "1" || (Deno.stderr.isTerminal() && !Deno.env.get("NO_COLOR"));

function paint(code: string, s: string): string {
  return consoleColor ? `\x1b[${code}m${s}\x1b[0m` : s;
}

function consoleFormatter(rec: log.LogRecord): string {
  const d = new Date(rec.datetime);
  const time = noTime
    ? ""
    : `${paint(
        "2",
        `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(
          2,
          "0",
        )}:${String(d.getSeconds()).padStart(2, "0")}.${String(d.getMilliseconds()).padStart(
          3,
          "0",
        )}`,
      )} `;
  // Unified terminal scheme (shared with the client): lowercase right-padded
  // level as the first word, then an optional lowercase dim module/scope (no
  // brackets), then the message. The file formatter keeps the UPPERCASE [scope]
  // machine shape.
  const level = paint(LEVEL_COLOR[rec.levelName] ?? "0", rec.levelName.toLowerCase().padEnd(5));
  const scope = rec.loggerName === "default" ? "" : `${paint("2", rec.loggerName.toLowerCase())} `;
  // Per-line head so a multi-line message keeps the level + scope on every line
  // (the dev multiplexer recognizes our lines by their leading level word).
  const head = `${time}${level} ${scope}`;
  return head + scrubSecrets(rec.msg).replace(/\n/g, `\n${head}`);
}

// @std/log's file handlers buffer writes (4 KB) and only flush when the buffer
// fills, on CRITICAL records, or at process end, so core.log trails reality by
// however long it takes to produce 4 KB of lines (and a killed process loses
// the tail entirely). Core's log volume is tiny; flush every record.
class EagerRotatingFileHandler extends log.RotatingFileHandler {
  override log(msg: string): void {
    super.log(msg);
    this.flush();
  }
}

export async function initLogger(): Promise<void> {
  if (initialized) return;

  await Deno.mkdir(paths().logsDir, { recursive: true });

  log.setup({
    handlers: {
      // useColors:false because consoleFormatter owns all coloring (gated on TTY /
      // LOCA_LOG_COLOR). Otherwise std wraps every line in its own level color.
      console: new log.ConsoleHandler("DEBUG", {
        formatter: consoleFormatter,
        useColors: false,
      }),
      file: new EagerRotatingFileHandler("INFO", {
        filename: paths().logFile,
        maxBytes: 10 * 1024 * 1024,
        maxBackupCount: 5,
        formatter: fileFormatter,
      }),
    },
    loggers: {
      default: { level: "DEBUG", handlers: ["console", "file"] },
    },
  });

  // Scoped loggers handed out before setup() were created handler-less (and
  // setup() wipes std's registry anyway), so wire every one of them to the
  // default logger's handlers now.
  for (const logger of scopedLoggers.values()) wireToDefault(logger);

  // Set AFTER setup so a setup() failure leaves the flag false and the
  // boot-failure catch can fall back to console.error.
  initialized = true;
}

export function isLoggerReady(): boolean {
  return initialized;
}

// Our own registry of scoped loggers. @std/log's named-logger registry is
// useless here: getLogger(name) on an unconfigured name returns a cached
// handler-less Logger (every line silently dropped), and setup() clears the
// registry while modules keep their stale import-time references. So we
// construct Logger instances ourselves and attach the default handlers,
// immediately when already initialized or from initLogger() otherwise.
const scopedLoggers = new Map<string, log.Logger>();

// Dependency scopes: loggers that relay third-party output (the sidecar
// binaries' stdout/stderr). Mirrors the Rust-side convention of gating
// dependency crates at info: their DEBUG chatter is dropped, INFO and above
// pass, and every scope of our own logs at DEBUG.
const DEPENDENCY_SCOPE_LEVELS: Record<string, log.LevelName> = {
  sidecars: "INFO",
};

function wireToDefault(logger: log.Logger): void {
  const def = log.getLogger();
  const dependencyLevel = DEPENDENCY_SCOPE_LEVELS[logger.loggerName];
  if (dependencyLevel) logger.levelName = dependencyLevel;
  else logger.level = def.level;
  // Shared array reference on purpose: a future handler swap on the default
  // logger propagates to every scope.
  logger.handlers = def.handlers;
}

export function getLogger(scope?: string): log.Logger {
  if (!scope) return log.getLogger();
  let logger = scopedLoggers.get(scope);
  if (!logger) {
    logger = new log.Logger(scope, "DEBUG", { handlers: [] });
    scopedLoggers.set(scope, logger);
    if (initialized) wireToDefault(logger);
  }
  return logger;
}
