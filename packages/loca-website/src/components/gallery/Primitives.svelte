<script lang="ts">
  import type { ComponentProps } from "svelte";
  // The primitives section of the gallery: a dedicated card per shared primitive
  // (A0), driven by PRIMITIVE_SAMPLES so each variant/state is shown at least
  // once. Children, callbacks, and anchors (which cannot live in a `.ts` sample)
  // are supplied here per primitive; the sample carries only the data props that
  // define the variant. Each non-overlay primitive renders inside a `surface`
  // panel (a `bg-surface` bubble floating on the focus grid), the on-surface
  // context these leaves ship in, so the theme flip resolves correctly. The three
  // overlays (Modal/ActionSheet/Popover) paint their own backdrop instead.
  // check-primitive-coverage asserts every primitives/*.svelte has a bundle here
  // and a registry entry.
  import { PRIMITIVE_SAMPLES } from "@loca/shared/ui/samples";
  import ActionSheet from "@loca/shared/ui/components/primitives/ActionSheet.svelte";
  import Alert from "@loca/shared/ui/components/primitives/Alert.svelte";
  import Bubble from "@loca/shared/ui/components/primitives/Bubble.svelte";
  import Button from "@loca/shared/ui/components/primitives/Button.svelte";
  import ButtonGroup from "@loca/shared/ui/components/primitives/ButtonGroup.svelte";
  import Card from "@loca/shared/ui/components/primitives/Card.svelte";
  import Checkbox from "@loca/shared/ui/components/primitives/Checkbox.svelte";
  import Chip from "@loca/shared/ui/components/primitives/Chip.svelte";
  import CollapsibleLabel from "@loca/shared/ui/components/primitives/CollapsibleLabel.svelte";
  import Expand from "@loca/shared/ui/components/primitives/Expand.svelte";
  import Expandable from "@loca/shared/ui/components/primitives/Expandable.svelte";
  import FlushSelect from "@loca/shared/ui/components/primitives/FlushSelect.svelte";
  import FormField from "@loca/shared/ui/components/primitives/FormField.svelte";
  import HelpText from "@loca/shared/ui/components/primitives/HelpText.svelte";
  import IconButton from "@loca/shared/ui/components/primitives/IconButton.svelte";
  import IconText from "@loca/shared/ui/components/primitives/IconText.svelte";
  import Input from "@loca/shared/ui/components/primitives/Input.svelte";
  import ListItem from "@loca/shared/ui/components/primitives/ListItem.svelte";
  import Markdown from "@loca/shared/ui/components/primitives/Markdown.svelte";
  import Modal from "@loca/shared/ui/components/primitives/Modal.svelte";
  import OptionCard from "@loca/shared/ui/components/primitives/OptionCard.svelte";
  import Popover from "@loca/shared/ui/components/primitives/Popover.svelte";
  import SearchInput from "@loca/shared/ui/components/primitives/SearchInput.svelte";
  import SectionHeader from "@loca/shared/ui/components/primitives/SectionHeader.svelte";
  import SubsectionHeader from "@loca/shared/ui/components/primitives/SubsectionHeader.svelte";
  import Select from "@loca/shared/ui/components/primitives/Select.svelte";
  import SidebarItem from "@loca/shared/ui/components/primitives/SidebarItem.svelte";
  import Slider from "@loca/shared/ui/components/primitives/Slider.svelte";
  import Tabs from "@loca/shared/ui/components/primitives/Tabs.svelte";
  import Textarea from "@loca/shared/ui/components/primitives/Textarea.svelte";
  import Toggle from "@loca/shared/ui/components/primitives/Toggle.svelte";
  import GalleryCard from "./GalleryCard.svelte";

  const noop = (): void => {};
  const entries = <T,>(o: Record<string, T>) => Object.entries(o);
  const P = PRIMITIVE_SAMPLES;

  let popoverAnchor = $state<HTMLElement | null>(null);
</script>

<section class="flex flex-col gap-6">
  <h2 class="text-lg font-medium">Primitives</h2>

  <div class="columns-1 sm:columns-2 lg:columns-3 gap-4">
    {#each entries(P.Button) as [name, p] (name)}
      <GalleryCard label={`Button · ${name}`} surface>
        <Button {...p as Omit<ComponentProps<typeof Button>, "children">} onclick={noop}
          >Button</Button
        >
      </GalleryCard>
    {/each}

    {#each entries(P.IconButton) as [name, p] (name)}
      <GalleryCard label={`IconButton · ${name}`} surface>
        <IconButton {...p as Omit<ComponentProps<typeof IconButton>, "children">} onclick={noop} />
      </GalleryCard>
    {/each}

    {#each entries(P.IconText) as [name, p] (name)}
      <GalleryCard label={`IconText · ${name}`} surface>
        <div class="w-56">
          <IconText {...p as Omit<ComponentProps<typeof IconText>, "children">}>
            {#if name === "header"}
              <code
                class="font-mono bg-accent-yellow-200 text-accent-yellow-700 rounded-small px-1.5 py-0.5"
                >read_file</code
              >
              wants to read a file
            {:else if name === "error"}
              Couldn't complete that action
            {:else}
              Summarized
            {/if}
          </IconText>
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.ButtonGroup) as [name, p] (name)}
      <GalleryCard label={`ButtonGroup · ${name}`} surface>
        <ButtonGroup {...p as Omit<ComponentProps<typeof ButtonGroup>, "children">}>
          <Button variant="ghost" onclick={noop}>One</Button>
          <Button variant="ghost" onclick={noop}>Two</Button>
        </ButtonGroup>
      </GalleryCard>
    {/each}

    {#each entries(P.Alert) as [name, p] (name)}
      <GalleryCard label={`Alert · ${name}`} surface>
        <div class="w-64">
          <Alert {...p as Omit<ComponentProps<typeof Alert>, "children">}
            >Something needs your attention.</Alert
          >
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Chip) as [name, p] (name)}
      <GalleryCard label={`Chip · ${name}`} surface>
        <Chip {...p as Omit<ComponentProps<typeof Chip>, "children">} />
      </GalleryCard>
    {/each}

    {#each entries(P.Checkbox) as [name, p] (name)}
      <GalleryCard label={`Checkbox · ${name}`} surface>
        <Checkbox
          {...p as Omit<ComponentProps<typeof Checkbox>, "children">}
          ariaLabel="Sample"
          onchange={noop}
        />
      </GalleryCard>
    {/each}

    {#each entries(P.Toggle) as [name, p] (name)}
      <GalleryCard label={`Toggle · ${name}`} surface>
        <Toggle
          {...p as Omit<ComponentProps<typeof Toggle>, "children">}
          onchange={noop}
          onselect={noop}
        />
      </GalleryCard>
    {/each}

    {#each entries(P.Input) as [name, p] (name)}
      <GalleryCard label={`Input · ${name}`} surface>
        <div class="w-56">
          <Input {...p as Omit<ComponentProps<typeof Input>, "children">} oninput={noop} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Textarea) as [name, p] (name)}
      <GalleryCard label={`Textarea · ${name}`} surface>
        <div class="w-64">
          <Textarea
            {...p as Omit<ComponentProps<typeof Textarea>, "children">}
            oninput={noop}
            minHeight="min-h-20"
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Select) as [name, p] (name)}
      <GalleryCard label={`Select · ${name}`} surface>
        <div class="w-56">
          <Select {...p as Omit<ComponentProps<typeof Select>, "children">} onchange={noop} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.FlushSelect) as [name, p] (name)}
      <GalleryCard label={`FlushSelect · ${name}`} surface>
        <FlushSelect
          {...p as Omit<ComponentProps<typeof FlushSelect>, "children">}
          onchange={noop}
        />
      </GalleryCard>
    {/each}

    {#each entries(P.SearchInput) as [name, p] (name)}
      <GalleryCard label={`SearchInput · ${name}`} surface>
        <div class="w-56">
          <SearchInput
            {...p as Omit<ComponentProps<typeof SearchInput>, "children">}
            oninput={noop}
            onclear={noop}
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Slider) as [name, p] (name)}
      <GalleryCard label={`Slider · ${name}`} surface>
        <div class="w-64">
          <Slider {...p as Omit<ComponentProps<typeof Slider>, "children">} oninput={noop} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Tabs) as [name, p] (name)}
      <GalleryCard label={`Tabs · ${name}`} surface>
        <div class="w-64">
          <Tabs {...p as Omit<ComponentProps<typeof Tabs>, "children">} onSelect={noop} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.OptionCard) as [name, p] (name)}
      <GalleryCard label={`OptionCard · ${name}`} surface>
        <div class="w-56">
          <OptionCard
            {...p as Omit<ComponentProps<typeof OptionCard>, "children">}
            onclick={noop}
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Card) as [name, p] (name)}
      <GalleryCard label={`Card · ${name}`} surface>
        <Card {...p as Omit<ComponentProps<typeof Card>, "children">}>
          <div class="text-sm text-default-700">Card content</div>
        </Card>
      </GalleryCard>
    {/each}

    {#each entries(P.Bubble) as [name, p] (name)}
      <!-- A Bubble is its own surface; it sits straight on the grid, not in a panel. -->
      <GalleryCard label={`Bubble · ${name}`}>
        <Bubble {...p as Omit<ComponentProps<typeof Bubble>, "children">}>A message bubble.</Bubble>
      </GalleryCard>
    {/each}

    {#each entries(P.ListItem) as [name, p] (name)}
      <GalleryCard label={`ListItem · ${name}`} surface>
        <div class="w-56">
          <ListItem {...p as Omit<ComponentProps<typeof ListItem>, "children">} onclick={noop}
            >List row</ListItem
          >
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.SidebarItem) as [name, p] (name)}
      <GalleryCard label={`SidebarItem · ${name}`} surface>
        <div class="w-48">
          <SidebarItem
            {...p as Omit<ComponentProps<typeof SidebarItem>, "children">}
            onclick={noop}
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.SectionHeader) as [name, p] (name)}
      <GalleryCard label={`SectionHeader · ${name}`} surface>
        <div class="w-64">
          <SectionHeader
            {...p as Omit<ComponentProps<typeof SectionHeader>, "children">}
            onToggle={noop}
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.SubsectionHeader) as [name, p] (name)}
      <GalleryCard label={`SubsectionHeader · ${name}`} surface>
        <div class="w-64">
          <SubsectionHeader {...p as Omit<ComponentProps<typeof SubsectionHeader>, "children">} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.FormField) as [name, p] (name)}
      <GalleryCard label={`FormField · ${name}`} surface>
        <div class="w-64">
          <FormField {...p as Omit<ComponentProps<typeof FormField>, "children">} onReset={noop}>
            <Input value="" placeholder="Value" oninput={noop} />
          </FormField>
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.HelpText) as [name, p] (name)}
      <GalleryCard label={`HelpText · ${name}`} surface>
        <div class="w-64">
          <HelpText {...p as Omit<ComponentProps<typeof HelpText>, "children">} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Markdown) as [name, p] (name)}
      <GalleryCard label={`Markdown · ${name}`} surface>
        <div class="w-64">
          <Markdown {...p as Omit<ComponentProps<typeof Markdown>, "children">} />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Expandable) as [name, p] (name)}
      <GalleryCard label={`Expandable · ${name}`} surface>
        <div class="w-64">
          <Expandable {...p as Omit<ComponentProps<typeof Expandable>, "children">}>
            {#snippet title()}
              <span class="text-sm font-medium text-default-700">Details</span>
            {/snippet}
            <p class="text-sm text-default-600">The disclosed body content.</p>
          </Expandable>
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Expand) as [name, p] (name)}
      <GalleryCard label={`Expand · ${name}`} surface>
        <div class="w-64 text-sm text-default-600">
          <Expand {...p as Omit<ComponentProps<typeof Expand>, "children">}>
            <p>Revealed content.</p>
          </Expand>
          {#if !(p as Omit<ComponentProps<typeof Expand>, "children">).open}
            <span class="text-default-400">(collapsed: nothing rendered)</span>
          {/if}
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.CollapsibleLabel) as [name, p] (name)}
      <GalleryCard label={`CollapsibleLabel · ${name}`} surface>
        <div class="flex items-center gap-1 text-sm text-default-700">
          <span class="i-material-symbols-folder-outline-rounded text-base"></span>
          <CollapsibleLabel {...p as Omit<ComponentProps<typeof CollapsibleLabel>, "children">}
            >Label</CollapsibleLabel
          >
        </div>
      </GalleryCard>
    {/each}

    <!-- Overlays render open over their own dimmed backdrop, pinned to the card. -->
    {#each entries(P.Modal) as [name, p] (name)}
      <GalleryCard label={`Modal · ${name}`} backdrop>
        <div class="relative h-72 w-full">
          <Modal
            {...p as Omit<ComponentProps<typeof Modal>, "children">}
            onclose={noop}
            ariaLabel="Sample modal"
          >
            <h3 class="text-base font-medium text-default-800">Dialog title</h3>
            <p class="text-sm text-default-600">A centered dialog over a dimmed backdrop.</p>
          </Modal>
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.ActionSheet) as [name, p] (name)}
      <GalleryCard label={`ActionSheet · ${name}`} backdrop>
        <div class="relative h-80 w-full">
          <ActionSheet
            {...p as Omit<ComponentProps<typeof ActionSheet>, "children">}
            onclose={noop}
            items={[
              {
                label: "Copy",
                icon: "i-material-symbols-content-copy-outline-rounded",
                onSelect: noop,
              },
              { label: "Edit", icon: "i-material-symbols-edit-outline-rounded", onSelect: noop },
              {
                label: "Delete",
                icon: "i-material-symbols-delete-outline-rounded",
                onSelect: noop,
                destructive: true,
              },
            ]}
          />
        </div>
      </GalleryCard>
    {/each}

    {#each entries(P.Popover) as [name, p] (name)}
      <GalleryCard label={`Popover · ${name}`} backdrop>
        <div class="relative h-56 w-full flex items-start justify-center pt-6">
          <button
            bind:this={popoverAnchor}
            class="rounded-medium bg-surface-inset px-3 py-1.5 text-sm text-default-700"
          >
            Anchor
          </button>
          <Popover
            {...p as Omit<ComponentProps<typeof Popover>, "children">}
            anchor={popoverAnchor}
            onclose={noop}
            ariaLabel="Sample popover"
          >
            <div class="p-3 text-sm text-default-700">Popover content</div>
          </Popover>
        </div>
      </GalleryCard>
    {/each}
  </div>
</section>
