<script lang="ts">
  import type { SettingField, SidecarsStatusResponse } from "@loca/shared";
  import { onMount } from "svelte";
  import { cores } from "$lib/core";
  import { serversState, settingsState } from "../../../state";
  import { platform } from "$lib/platform";
  import { formatBytes } from "$lib/util/format";
  import { getLogger } from "$lib/util/log";
  import FieldCard from "./FieldCard.svelte";
  import ServicesFieldView from "@loca/shared/ui/components/settings/ServicesFieldView.svelte";

  const log = getLogger("services");

  let {
    field,
    scope,
    horizontal = false,
  } = $props<{
    field: SettingField;
    scope: "client" | "core";
    horizontal?: boolean;
  }>();

  type ServiceKey = "llama" | "speech" | "tool";
  const SERVICES: ServiceKey[] = ["llama", "speech", "tool"];

  // Core-scope metrics (core process + sidecars); client-scope metrics (the
  // desktop app process). Each polled only while the field is on-screen.
  let metrics = $state<SidecarsStatusResponse | null>(null);
  let mainMetrics = $state<{ rssMb: number; cpuPct: number } | null>(null);
  let rootEl = $state<HTMLDivElement | null>(null);

  // Wider metric columns when the settings panel is laid out horizontally.
  const cpuWidthClass = $derived(horizontal ? "w-16" : "w-12");
  const ramWidthClass = $derived(horizontal ? "w-20" : "w-16");

  function labelFor(key: ServiceKey): string {
    switch (key) {
      case "llama":
        return "Language Model";
      case "speech":
        return "Speech";
      case "tool":
        return "Tools";
    }
  }

  // Channel-aware default sidecar ports (latest → 7711/7712), resolved from the
  // platform on mount. Until then, show the stable defaults.
  let defaultLlmPort = $state("7701");
  let defaultSttPort = $state("7702");
  onMount(() => {
    if (scope !== "core") return;
    void platform()
      .pairing.localSidecarPorts()
      .then((p) => {
        defaultLlmPort = String(p.llm);
        defaultSttPort = String(p.stt);
      })
      .catch((e) => log.debug("localSidecarPorts failed", e));
  });

  function endpointFor(key: ServiceKey): string | null {
    const s = settingsState.currentSettings;
    if (key === "llama") {
      return `${s["llm.host"] || "127.0.0.1"}:${s["llm.port"] || defaultLlmPort}`;
    }
    if (key === "speech") {
      // The speech sidecar binds a fixed loopback port (no user host/port knob).
      return `127.0.0.1:${defaultSttPort}`;
    }
    return null;
  }

  function statusFor(key: ServiceKey): string {
    return serversState.serverStatuses[key]?.status ?? "Disabled";
  }

  function sidecarMetric(
    key: ServiceKey,
  ): { status?: string; rssMb?: number; cpuPct?: number } | undefined {
    return metrics?.sidecars.find((s) => s.kind === key);
  }

  type Row = {
    label: string;
    sub?: string;
    rssMb?: number;
    cpuPct?: number;
    // Set for a supervised sidecar in Error, so the row offers a Retry that
    // re-applies its boot decision (and clears the crash flap-guard).
    retryKind?: "llama" | "speech";
  };

  let retrying = $state<"llama" | "speech" | null>(null);
  async function retry(kind: "llama" | "speech") {
    retrying = kind;
    try {
      await cores().api().sidecars.restart(kind);
      await refresh();
    } catch (e) {
      log.warn("sidecar restart failed", e);
    } finally {
      retrying = null;
    }
  }

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if (scope === "client") {
      if (mainMetrics) {
        out.push({
          label: "Main Application",
          rssMb: mainMetrics.rssMb,
          cpuPct: mainMetrics.cpuPct,
        });
      }
      return out;
    }
    // core scope: the core process + its sidecars.
    if (metrics?.core) {
      out.push({
        label: "Core Service",
        rssMb: metrics.core.rssMb,
        cpuPct: metrics.core.cpuPct,
      });
    }
    for (const key of SERVICES) {
      // tool is an ephemeral per-call worker with no persistent process.
      if (key === "tool") continue;
      const status = statusFor(key);
      if (status === "Disabled") continue;
      const m = sidecarMetric(key);
      const endpoint = endpointFor(key);
      out.push({
        label: labelFor(key),
        sub: `${status}${endpoint ? ` · ${endpoint}` : ""}`,
        rssMb: m?.rssMb,
        cpuPct: m?.cpuPct,
        // tool is skipped above, so an errored sidecar here is llama or speech.
        retryKind: status === "Error" ? (key as "llama" | "speech") : undefined,
      });
    }
    return out;
  });

  const totalRss = $derived(rows.reduce((s, r) => s + (r.rssMb ?? 0), 0));
  const totalCpu = $derived(rows.reduce((s, r) => s + (r.cpuPct ?? 0), 0));

  function fmtCpu(pct: number | undefined): string {
    return pct === undefined ? "-" : `${pct.toFixed(1)}%`;
  }
  function fmtRam(mb: number | undefined): string {
    return mb === undefined ? "-" : formatBytes(mb * 1024 * 1024);
  }

  // Rows pre-formatted for the presentational View: every CPU/RAM value becomes
  // a display string here, and an errored row carries its retry label/disabled
  // state plus the opaque `retryKind` the View echoes back through `onRetry`.
  const viewRows = $derived(
    rows.map((r) => ({
      label: r.label,
      sub: r.sub,
      cpuText: fmtCpu(r.cpuPct),
      ramText: fmtRam(r.rssMb),
      retryKind: r.retryKind,
      retryLabel: r.retryKind ? (retrying === r.retryKind ? "Retrying..." : "Retry") : undefined,
      retryDisabled: retrying !== null,
    })),
  );
  const totalCpuText = $derived(rows.length > 0 ? `${totalCpu.toFixed(1)}%` : "-");
  const totalRamText = $derived(rows.length > 0 ? formatBytes(totalRss * 1024 * 1024) : "-");

  async function refresh() {
    if (scope === "client") {
      try {
        const self = await platform().process.selfMetrics();
        // pid 0 (web stub) means the platform can't measure itself; drop the row.
        mainMetrics = self.pid !== 0 ? { rssMb: self.rssMb, cpuPct: self.cpuPct } : null;
      } catch (e) {
        log.debug("selfMetrics failed", e);
      }
      return;
    }
    // No paired/selected core yet: nothing to poll (cores().api() would throw).
    if (!cores().currentClient()) return;
    try {
      metrics = await cores().api().sidecars.status();
    } catch (e) {
      log.debug("metrics refresh failed", e);
    }
  }

  // Poll every 2s, but only while the field is actually visible (the settings
  // panel is tall; no point sampling sysinfo when scrolled away).
  $effect(() => {
    if (!rootEl) return;
    let intervalId: ReturnType<typeof setInterval> | null = null;
    const start = () => {
      if (intervalId !== null) return;
      void refresh();
      intervalId = setInterval(() => void refresh(), 2000);
    };
    const stop = () => {
      if (intervalId === null) return;
      clearInterval(intervalId);
      intervalId = null;
    };
    const io = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) start();
        else stop();
      },
      { threshold: 0 },
    );
    io.observe(rootEl);
    return () => {
      io.disconnect();
      stop();
    };
  });
</script>

<FieldCard {field}>
  <div bind:this={rootEl}>
    <ServicesFieldView
      rows={viewRows}
      {totalCpuText}
      {totalRamText}
      {cpuWidthClass}
      {ramWidthClass}
      onRetry={(kind) => retry(kind as "llama" | "speech")}
    />
  </div>
</FieldCard>
