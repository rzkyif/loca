/**
 * Builds the command-line arguments used to launch the LLM sidecar from the
 * user's current settings. The same definition is used to generate the actual
 * args passed to the process and the preview shown in the settings screen.
 */

import { platform } from "$lib/platform";

export type CommandType = "llm";

export interface CommandArg {
  flag: string;
  settingId: string;
  argType: "value" | "quoted" | "boolean";
  omitEmpty?: boolean;
  falseFlag?: string;
  isModelPath?: boolean;
  isMmprojPath?: boolean;
  /** Only include this arg when the given setting is truthy */
  enabledBy?: string;
}

export interface CommandDefinition {
  binary: string;
  args: CommandArg[];
}

export const COMMANDS: Record<CommandType, CommandDefinition> = {
  llm: {
    binary: "loca-llama-server",
    args: [
      {
        flag: "-m",
        settingId: "llm.modelPath",
        argType: "quoted",
        omitEmpty: true,
        isModelPath: true,
      },
      { flag: "-c", settingId: "llm.contextSize", argType: "value" },
      { flag: "--mmap", settingId: "llm.mmap", argType: "boolean" },
      { flag: "-t", settingId: "llm.threads", argType: "value" },
      {
        flag: "--n-gpu-layers",
        settingId: "llm.gpuLayers",
        argType: "value",
        omitEmpty: true,
      },
      { flag: "--flash-attn", settingId: "llm.flashAttn", argType: "boolean" },
      { flag: "--host", settingId: "llm.host", argType: "value" },
      { flag: "--port", settingId: "llm.port", argType: "value" },
      {
        flag: "--webui",
        settingId: "llm.webui",
        argType: "boolean",
        falseFlag: "--no-webui",
      },
      {
        flag: "--mmproj",
        settingId: "llm.mmprojPath",
        argType: "quoted",
        omitEmpty: true,
        isMmprojPath: true,
        enabledBy: "llm.supportImages",
      },
    ],
  },
};

export function getCommandSettingIds(type: CommandType): string[] {
  return COMMANDS[type].args.map((a) => a.settingId);
}

function isEmpty(v: any): boolean {
  return v === "" || v === undefined || v === null || (typeof v === "string" && v.trim() === "");
}

export function buildArgs(type: CommandType, currentSettings: Record<string, any>): string[] {
  const cmd = COMMANDS[type];
  const args: string[] = [];
  for (const arg of cmd.args) {
    if (arg.enabledBy && !currentSettings[arg.enabledBy]) continue;
    const val = currentSettings[arg.settingId];
    if (arg.argType === "boolean") {
      if (val) args.push(arg.flag);
      else if (arg.falseFlag) args.push(arg.falseFlag);
    } else {
      if (!arg.omitEmpty || !isEmpty(val)) {
        args.push(arg.flag);
        args.push(
          arg.isModelPath
            ? "__MODEL_PATH__"
            : arg.isMmprojPath
              ? "__MMPROJ_PATH__"
              : String(val).trim(),
        );
      }
    }
  }
  return args;
}

export async function buildPreview(
  type: CommandType,
  currentSettings: Record<string, any>,
): Promise<string> {
  const cmd = COMMANDS[type];
  const parts = [cmd.binary];
  for (const arg of cmd.args) {
    if (arg.enabledBy && !currentSettings[arg.enabledBy]) continue;
    let v = currentSettings[arg.settingId];

    if ((arg.isModelPath || arg.isMmprojPath) && typeof v === "string") {
      if (v.startsWith("@")) {
        const hfMatch = v.match(/^@([^/]+)\/([^/]+)\/([^/]+)\/(.+)$/);
        if (hfMatch) {
          const [, username, reponame, , filename] = hfMatch;
          v = `~/.loca/models/${username}/${reponame}/${filename}`;
        }
      }
      try {
        v = await platform().resolvePath(v);
      } catch {
        /* leave v as-is on resolution failure */
      }
    }

    if (arg.argType === "boolean") {
      if (v) parts.push(arg.flag);
      else if (arg.falseFlag) parts.push(arg.falseFlag);
    } else if (arg.argType === "quoted") {
      if (!arg.omitEmpty || !isEmpty(v)) {
        parts.push(`${arg.flag} "${String(v).trim()}"`);
      }
    } else {
      if (!arg.omitEmpty || !isEmpty(v)) {
        parts.push(`${arg.flag} ${String(v).trim()}`);
      }
    }
  }
  return parts.join(" \\\n  ");
}
