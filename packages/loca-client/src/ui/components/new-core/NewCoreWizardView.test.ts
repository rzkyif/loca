// The wizard's first step on a build that carries a Core of its own.
//
// The two branches are the whole feature, and they are not a primary action
// plus a fallback: a phone can genuinely go either way. So the assertions are
// about both being present, both being reachable, and neither being wired to
// the other's callback, which is the failure a reader cannot see in the markup.
//
// The component under test is the SHARED one the website gallery renders, per
// the single-source-UI rule; the client is where the test lives because that is
// where the component-test harness is.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import NewCoreWizardView from "@loca/shared/ui/components/new-core/NewCoreWizardView.svelte";

const DOCS = "https://loca.rzkyif.com/manual/maintenance/cores";

function renderChoose(props: Record<string, unknown> = {}) {
  return render(NewCoreWizardView, {
    props: { step: "choose", coreSetupDocsUrl: DOCS, ...props },
  });
}

/** Rendered text with layout whitespace collapsed, so an assertion about a
 *  sentence is not really an assertion about where the markup wraps. */
function text(container: HTMLElement): string {
  return (container.textContent ?? "").replace(/\s+/g, " ").trim();
}

function buttonWith(container: HTMLElement, label: string): HTMLElement {
  const el = [...container.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(label),
  );
  if (!el) throw new Error(`no button containing "${label}"`);
  return el;
}

describe("the new-core wizard's fork step", () => {
  it("offers both places a Core can run", () => {
    const { container } = renderChoose();
    expect(text(container)).toContain("Use This Device");
    expect(text(container)).toContain("Use Another Device");
  });

  it("wires each branch to its own callback and only its own", async () => {
    const onUseThisDevice = vi.fn();
    const onUseAnotherDevice = vi.fn();
    const { container } = renderChoose({ onUseThisDevice, onUseAnotherDevice });

    await fireEvent.click(buttonWith(container, "Use This Device"));
    expect(onUseThisDevice).toHaveBeenCalledTimes(1);
    expect(onUseAnotherDevice).not.toHaveBeenCalled();

    await fireEvent.click(buttonWith(container, "Use Another Device"));
    expect(onUseAnotherDevice).toHaveBeenCalledTimes(1);
    expect(onUseThisDevice).toHaveBeenCalledTimes(1);
  });

  it("disables both while an adoption is in flight, and says what is happening", () => {
    const { container } = renderChoose({ busy: "claiming" });
    expect(buttonWith(container, "Use This Device")).toBeDisabled();
    expect(buttonWith(container, "Use Another Device")).toBeDisabled();
    expect(text(container)).toContain("Starting the Core on this device");
  });

  it("shows no pinned mobile footer: the two branches ARE the actions", () => {
    // Every other step has exactly one primary action and pins it to a footer.
    // A footer here would be an empty bar, or worse, a third action implying one
    // of the two branches is the default.
    const { container } = renderChoose({ onMobile: true, locked: true });
    expect(text(container)).not.toContain("Check Connection");
    expect(text(container)).not.toContain("Pair");
  });

  it("greets on a locked first run and states its own lead otherwise", () => {
    const first = renderChoose({ onMobile: true, locked: true });
    expect(text(first.container)).toContain("Welcome to Loca");
    expect(text(first.container)).toContain("Choose where yours runs");

    // Reached from Settings > Cores: no welcome, so the step carries the lead.
    const added = renderChoose({ onMobile: true, locked: false });
    expect(text(added.container)).not.toContain("Welcome to Loca");
    expect(text(added.container)).toContain("Choose where this one runs");
  });

  it("surfaces an error from a failed adoption", () => {
    const { container } = renderChoose({ error: "no Core inside this build" });
    expect(text(container)).toContain("no Core inside this build");
  });
});
