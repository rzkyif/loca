// Entry module the Loca extension worker imports. Each tool's function is
// declared in loca.json and exported by name here. This is the dev-only
// samples extension: one small tool per capability, for authors to learn from.

export {
  sampleChoice,
  sampleDiff,
  sampleFiles,
  sampleImage,
  sampleSchedule,
  sampleStt,
  sampleTable,
} from "./src/ask.ts";
export { sampleDisplay } from "./src/display.ts";
export { sampleDatabase } from "./src/database.ts";
export { sampleLlm, sampleMemory, sampleTts } from "./src/capabilities.ts";
