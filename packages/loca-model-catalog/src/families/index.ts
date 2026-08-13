// All authored model families. Adding a family is: write one file here and add
// it to this list.

import type { ModelFamily } from "@loca/shared";
import { qwen36 } from "./qwen-3.6.ts";
import { qwen35 } from "./qwen-3.5.ts";
import { gemma4 } from "./gemma-4.ts";

export const FAMILIES: ModelFamily[] = [qwen36, qwen35, gemma4];
