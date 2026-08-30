import type { SettingGroup } from "../types.ts";
import { externalModelSection } from "./factories.ts";
import { DEFAULT_CONTEXT_SIZE, localThinkingBudget } from "../../quick-controls.ts";
import { DEFAULT_SAMPLING } from "../../recommend.ts";

// Static fallbacks for the local model fields, mirroring the Smallest-floor
// catalog model. They hold only until the Smart Preset resolves the
// hardware-fit values (and for external/custom setups).
const BASELINE_SAMPLING = {
  temperature: DEFAULT_SAMPLING.temperature,
  topP: 0.95,
  topK: 20,
  minP: 0,
  repeatPenalty: 1.0,
  dryMultiplier: DEFAULT_SAMPLING.dryMultiplier ?? 0,
  presencePenalty: DEFAULT_SAMPLING.presencePenalty ?? 0,
} as const;

export const llmGroup: SettingGroup = {
  id: "llm",
  // Hybrid. What the shared core loads (the model, its server, the vision /
  // idle-unload gates) is a core resource; the per-message sampling and
  // thinking knobs are per-client preferences the core applies per turn.
  destination: ["client-on-core", "core"],
  name: "Language Model",
  description:
    "The model that powers your chats. Run one locally on the core's device, or connect to an external API.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-psychology-rounded",
  iconInactive: "i-material-symbols-psychology-outline-rounded",
  sections: [
    {
      label: "General",
      destination: "core",
      fields: [
        {
          id: "llm.supportImages",
          name: "Image Attachments",
          description: "Let yourself attach images to messages. The model must support vision.",
          type: "boolean",
          // Off until asked for: turning it on makes the projector a required
          // download (205 MB to 1.2 GB). Requirement computation and the mobile
          // fit budget read this key as the user's answer, not a capability.
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.idleUnloadSeconds",
          name: "Unload When Idle",
          description:
            "Free up RAM by unloading the model after this long unused. It reloads on your next message; 0 keeps it loaded.",
          type: "number",
          defaultValue: 0,
          suffix: "s",
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Display",
      destination: "client-on-core",
      fields: [
        {
          id: "llm.showReasoning",
          name: "Show Thinking",
          description:
            "Show the model's thinking above each reply, when it thinks.\nSaved with the session but never sent back to the model.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Model",
      destination: "core",
      fields: [
        {
          id: "llm.provider",
          name: "Provider",
          description:
            "Run the model on the core's device (private, no internet) or connect to an external API.",
          type: "select",
          defaultValue: "local",
          options: [
            { value: "local", label: "Local" },
            { value: "external", label: "External" },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "llm.preset",
          name: "Smart Preset",
          visibleWhen: { field: "llm.provider", eq: "local" },
          description:
            "Three ready-made sizes (Smallest, Balanced, Smartest), each filled with the best-fitting model for your hardware. Pick one, or choose Custom to set the model and tuning yourself.",
          type: "model_preset",
          defaultValue: "smallest",
          presetConfig: {
            // The adaptive presets compute their values at runtime (no static
            // `defaults`), so list the keys they manage: editing one flips the
            // preset to Custom.
            managedKeys: [
              "llm.modelPath",
              "llm.mmprojPath",
              "llm.contextSize",
              "llm.threads",
              "llm.gpuLayers",
              "llm.flashAttn",
              "llm.idleUnloadSeconds",
              // Image Attachments, thinking budget, and temperature are
              // deliberately NOT managed: they are the user's call and must not
              // flip the preset to Custom. Model select still sets them as a
              // side effect. Only the model-defining tuning below flips it.
              "llm.topP",
              "llm.topK",
              "llm.minP",
              "llm.repeatPenalty",
              "llm.dryMultiplier",
              "llm.presencePenalty",
              "llm.samplers",
            ],
            // Placeholders: the client fills each card from
            // GET /api/v1/models/recommend, and selecting one calls
            // POST /api/v1/models/select { bucket }.
            options: [
              {
                id: "smallest",
                label: "Smallest",
                title: "Smallest",
                description: "The lightest capable model. Runs on almost anything.",
              },
              {
                id: "half",
                label: "Half",
                title: "Balanced",
                description:
                  "A capable model that uses about half your RAM. A good everyday default.",
              },
              {
                id: "full",
                label: "Full",
                title: "Smartest",
                description:
                  "The largest model your device can handle. Unloads itself after 5 minutes idle to free RAM.",
              },
            ],
            secondaryOptions: [
              {
                id: "custom",
                label: "Custom",
                title: "Custom",
                badges: [
                  {
                    icon: "i-material-symbols-build-rounded",
                    label: "Manual setup",
                  },
                ],
                description:
                  "Pick a specific model from the catalog, or configure the model path, context size, threads, and ports yourself.",
              },
            ],
          },
        },
      ],
    },
    {
      label: "Model Behavior",
      destination: "client-on-core",
      defaultCollapsed: true,
      fields: [
        {
          id: "llm.reasoning",
          name: "Thinking",
          description:
            "Whether the model should think before answering. The model may still skip it for simple prompts.",
          type: "select",
          defaultValue: "off",
          options: [
            { value: "off", label: "Off" },
            { value: "on", label: "On" },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "llm.reasoningBudget",
          name: "Thinking Budget",
          description: "How many tokens the model may spend thinking.",
          type: "number",
          // "Low" (1/16 of the default context), so enabling thinking lands on
          // a modest budget rather than an open-ended "Unlimited".
          defaultValue: localThinkingBudget("low", DEFAULT_CONTEXT_SIZE),
          visibleWhen: {
            allOf: [
              { field: "llm.provider", eq: "local" },
              { field: "llm.reasoning", neq: "off" },
            ],
          },
          optional: true,
          placeholder: "optional",
          descriptionTier: "ondemand",
        },
        {
          id: "llm.reasoningEffort",
          name: "Thinking Effort",
          description:
            "How hard the model should think. External providers take an effort level rather than a token budget.",
          type: "select",
          defaultValue: "high",
          // The OpenAI API's reasoning_effort levels, sent verbatim.
          options: [
            { value: "minimal", label: "Minimal" },
            { value: "low", label: "Low" },
            { value: "medium", label: "Medium" },
            { value: "high", label: "High" },
          ],
          visibleWhen: {
            allOf: [
              { field: "llm.provider", eq: "external" },
              { field: "llm.reasoning", neq: "off" },
            ],
          },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.temperature",
          name: "Temperature",
          description:
            "How varied the model's word choices are. Lower is more focused and predictable; higher is more creative.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.temperature,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.topP",
          name: "Top P",
          description: "Narrows word choices to the most likely options. Lower is more focused.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.topP,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.topK",
          name: "Top K",
          description:
            "Narrows word choices to this many of the most likely options. 0 turns it off.",
          type: "number",
          defaultValue: BASELINE_SAMPLING.topK,
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.minP",
          name: "Min P",
          description: "Drops unlikely word choices. Higher is more focused; 0 turns it off.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.minP,
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.repeatPenalty",
          name: "Repeat Penalty",
          description:
            "How strongly to discourage the model repeating itself. 1 is off; higher reduces repetition.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.repeatPenalty,
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.dryMultiplier",
          name: "Loop Guard",
          description:
            "Discourages the model from repeating whole phrases, which helps it avoid getting stuck in thinking loops. Higher is stronger; 0 turns it off.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.dryMultiplier,
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.presencePenalty",
          name: "Presence Penalty",
          description:
            "Nudges the model toward new words instead of ones it already used. Higher is more varied; 0 turns it off.",
          type: "float",
          defaultValue: BASELINE_SAMPLING.presencePenalty,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.samplers",
          name: "Sampler Order",
          description:
            "The order the model server runs its samplers in, separated by semicolons. Leave blank to use the model's default order.",
          type: "string",
          defaultValue: "",
          placeholder: "top_k;typ_p;top_p;min_p;xtc;temperature;dry;penalties",
          optional: true,
          visibleWhen: { field: "llm.provider", eq: "local" },
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Model Server Configuration",
      destination: "core",
      visibleWhen: { field: "llm.provider", eq: "local" },
      defaultCollapsed: true,
      fields: [
        {
          id: "llm.modelPath",
          name: "Model File",
          description: "Path to the model file to load, e.g. @user/repo/branch/model.gguf.",
          type: "string",
          // Smallest-floor catalog model; the Smart Preset overwrites it on
          // first run with the hardware-fit pick.
          defaultValue: "@unsloth/Qwen3.5-2B-GGUF/main/Qwen3.5-2B-Q4_K_M.gguf",
          descriptionTier: "ondemand",
        },
        {
          id: "llm.mmprojPath",
          name: "Vision File",
          description: "Path to the vision add-on file for image support.",
          type: "string",
          defaultValue: "@unsloth/Qwen3.5-2B-GGUF/main/mmproj-F16.gguf",
          placeholder: "@user/repo/branch/mmproj-f16.gguf",
          visibleWhen: { field: "llm.supportImages", eq: true },
          optionalWhen: { field: "llm.supportImages", eq: false },
          descriptionTier: "ondemand",
        },
        {
          id: "llm.contextSize",
          name: "Context Window",
          description:
            "How much text the model can consider at once, in tokens. Larger uses more RAM.",
          type: "number",
          defaultValue: 4096,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.threads",
          name: "CPU Threads",
          description: "How many CPU threads to use.",
          type: "number",
          defaultValue: 4,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.gpuLayers",
          name: "GPU Layers",
          description:
            "How much of the model to run on the GPU. Higher is faster if it fits; blank lets Loca decide.",
          type: "number",
          defaultValue: "",
          optional: true,
          placeholder: "auto",
          descriptionTier: "ondemand",
        },
        {
          id: "llm.binaryBackend",
          name: "Acceleration",
          visibleWhen: { field: "llm.provider", eq: "local" },
          description:
            "Which model-server build to download and run. Auto picks the best for your GPU; choose a specific backend to override, or CPU to stay off the GPU.",
          type: "select",
          defaultValue: "auto",
          options: [
            { value: "auto", label: "Auto" },
            { value: "cpu", label: "CPU" },
            { value: "vulkan", label: "Vulkan" },
            { value: "cuda", label: "CUDA (NVIDIA)" },
            { value: "rocm", label: "ROCm (AMD)" },
            { value: "hip", label: "HIP (AMD)" },
            { value: "metal", label: "Metal (Apple)" },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "llm.flashAttn",
          name: "Flash Attention",
          description: "Faster, lighter processing on supported hardware.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.mmap",
          // On by default: turning it off makes the weight pages resident
          // anonymous memory rather than file-backed and evictable, which on a
          // phone invites an OS kill. NOT `desktopOnly`: it governs the machine
          // the model loads on, not the one rendering this panel, so a phone
          // driving a desktop Core must still reach it.
          name: "Memory-Mapped Loading",
          description:
            "Load the model straight from disk for faster startup and much lower RAM use. Turning it off can crash the core on a phone.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.host",
          name: "Host",
          description: "Address the local model server binds to.",
          type: "string",
          defaultValue: "127.0.0.1",
          regex: [
            {
              regex:
                "^((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$",
              errorMessage: "Must be a valid IPv4 address",
            },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "llm.port",
          name: "Port",
          description: "Port the local model server listens on.",
          type: "string",
          defaultValue: "7701",
          regex: [
            { regex: "^\\d+$", errorMessage: "Port must be a number" },
            {
              regex:
                "^(?:[1-9]\\d{0,4}|[1-5]\\d{4}|6[0-4]\\d{3}|65[0-4]\\d{2}|655[0-2]\\d|6553[0-5])$",
              errorMessage: "Port must be 1-65535",
            },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "llm.webui",
          name: "llama.cpp Web UI",
          description: "Enable llama.cpp's built-in web interface.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "llm.commandPreview",
          name: "Command Preview",
          description: "",
          type: "command_preview",
          defaultValue: "",
          commandType: "llm",
        },
      ],
    },
    externalModelSection({
      idPrefix: "llm.external",
      label: "External Provider",
      visibleWhen: { field: "llm.provider", eq: "external" },
      baseUrlDescription:
        "Your provider's API endpoint. Must be HTTPS (HTTP allowed only for localhost).",
      apiKeyDescription: "Your provider API key. Stored securely in your device keychain.",
      model: {
        description: "The model name to use, e.g. gpt-4o-mini.",
        placeholder: "gpt-4o-mini",
      },
      embedModel: {
        description:
          "Optional model your External provider uses to surface relevant Memories and Tools, e.g. text-embedding-3-small. Reuses the Base URL and API Key above; leave blank to turn it off.",
        placeholder: "text-embedding-3-small",
      },
      contextSize: true,
    }),
  ],
};
