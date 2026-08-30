<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import Cursor from "./Cursor.svelte";
  import SubtitleBand from "./SubtitleBand.svelte";
  import { Demo, type SizedStageProps, type Timeline } from "../../lib/showcase";

  const ui = useUiContext();

  let { register, reportHeight }: SizedStageProps = $props();

  // No real audio: the subtitle band conveys both halves of the voice loop.
  // Voice Input captures the user's speech and, with Send After Dictation on by
  // default, sends the transcript automatically once it finishes; Text-to-Speech
  // reads the reply aloud, the band tracking the spoken words in sync with the
  // streaming bubble.
  const TRANSCRIPT = "What can I use instead of buttermilk?";
  const ANSWER =
    "Stir a tablespoon of lemon juice into a cup of milk and let it sit for five minutes. That gives you a quick buttermilk substitute.";

  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;
  // Spoken playback runs slower than the decode stream so the caption reads at a
  // speech-like cadence rather than the model's faster token rate.
  const SPEAK_SECONDS = ANSWER.length * 0.04;

  const noop = (): void => {};

  // The voice button's visible states mirror the client (UserInput.svelte): an
  // outline mic when idle, a filled blue mic once Voice Input is armed (waiting
  // for speech), and a green loading-loop while speech is being captured.
  const PLACEHOLDER_IDLE = "Enter your instructions...";
  const VOICE_CLASS_IDLE = "text-default-700";
  const VOICE_CLASS_WAITING = "text-accent-blue-400";
  const VOICE_CLASS_LISTENING = "text-accent-green-500";

  // Reserved height for the bottom caption-band row (matches the `h-20` slot),
  // added on top of the conversation column so the band never crowds the input.
  const BAND_ROW_H = 80;

  // Mock state, driven by the timeline.
  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showUser = $state(false);
  let showAnswer = $state(false);
  // Gate bubble entry animations off while measuring full content height (an
  // animating bubble starts collapsed at max-height 0 and would mis-measure).
  let measuring = $state(false);

  // Subtitle band.
  let showBand = $state(false);
  let bandSpeaker = $state<"user" | "agent">("user");
  let bandText = $state("");
  let bandActive = $state(false);

  // Voice Input button state (mirrors the client's vadEnabled/vadListening trio).
  let placeholder = $state(PLACEHOLDER_IDLE);
  let vadEnabled = $state(false);
  let vadListening = $state(false);
  let voiceClass = $state(VOICE_CLASS_IDLE);

  // Agent bubble Text-to-Speech border: `active` shows the border, `pulse` makes
  // it ping while audio is still being synthesised for this bubble.
  let ttsActive = $state(false);
  let ttsPulse = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  function reset(): void {
    inputValue = "";
    userText = "";
    agentText = "";
    showUser = false;
    showAnswer = false;
    showBand = false;
    bandSpeaker = "user";
    bandText = "";
    bandActive = false;
    placeholder = PLACEHOLDER_IDLE;
    vadEnabled = false;
    vadListening = false;
    voiceClass = VOICE_CLASS_IDLE;
    ttsActive = false;
    ttsPulse = false;
  }

  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser = true;
    showAnswer = true;
    userText = TRANSCRIPT;
    agentText = ANSWER;
    await tick();
    if (columnEl) reportHeight(columnEl.offsetHeight + BAND_ROW_H);
    reset();
    measuring = false;
    await tick();
  }

  onMount(() => {
    let cancelled = false;
    let timeline: Timeline | undefined;
    void (async () => {
      if (!stageEl || !cursorRef) return;
      const demo = new Demo(cursorRef, stageEl);
      // Rest the cursor at its inactive centre before measuring, so it is in
      // place on the first paint instead of jumping there once measuring ends.
      demo.placeFrac(0.5, 0.6);
      await measureContentHeight();
      if (cancelled) return;

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const voice = 'button[title="Voice Input"]';

      // 1. Voice Input armed: tap the mic; the button turns blue and the input
      //    waits for speech (vadEnabled, not yet listening).
      demo.move(tl, voice, { duration: 0.8 });
      demo.hover(tl, voice, true);
      demo.click(tl, voice, () => {
        vadEnabled = true;
        voiceClass = VOICE_CLASS_WAITING;
        placeholder = "Waiting for speech...";
      });
      demo.hover(tl, voice, false);
      demo.hold(tl, 0.6);

      // 2. Speech detected: the button goes green (loading-loop) and the band
      //    shows the live transcript as it is captured.
      tl.add(() => {
        vadListening = true;
        voiceClass = VOICE_CLASS_LISTENING;
        placeholder = "Listening...";
        showBand = true;
        bandSpeaker = "user";
        bandActive = true;
        bandText = "";
      });
      demo.type(tl, (v) => (bandText = v), TRANSCRIPT, { duration: 2.2 });
      demo.hold(tl, 0.3);

      // 3. Speech ends: VAD stops listening (back to blue) while the clip is
      //    transcribed; the caption freezes (no longer pinging).
      tl.add(() => {
        vadListening = false;
        voiceClass = VOICE_CLASS_WAITING;
        placeholder = "Transcribing...";
        bandActive = false;
      });
      demo.hold(tl, 0.8);

      // 4. Transcription finishes. With Send After Dictation on (the default),
      //    the finalized transcript is sent automatically, no Send tap needed:
      //    Voice Input switches off and the user's message appears on its own.
      tl.add(() => {
        vadEnabled = false;
        voiceClass = VOICE_CLASS_IDLE;
        placeholder = PLACEHOLDER_IDLE;
        showBand = false;
        userText = TRANSCRIPT;
        showUser = true;
      });
      demo.hold(tl, 0.6);

      // 5. Text-to-Speech, generation stage: the reply streams in while its
      //    bubble border PINGS, signalling audio is being synthesised for it.
      tl.add(() => {
        showAnswer = true;
        ttsActive = true;
        ttsPulse = true;
      });
      demo.type(tl, (v) => (agentText = v), ANSWER, { duration: ANSWER_SECONDS });
      // A short synth tail after the text finishes, border still pinging.
      demo.hold(tl, 0.5);

      // 6. Playback stage: audio plays, the border goes steady (no longer
      //    pinging), and the band reads the reply aloud at a spoken cadence.
      tl.add(() => {
        ttsPulse = false;
        showBand = true;
        bandSpeaker = "agent";
        bandActive = true;
        bandText = "";
      });
      demo.type(tl, (v) => (bandText = v), ANSWER, { duration: SPEAK_SECONDS });
      // Speech finished: clear the TTS border and stop the band pinging; the
      // caption stays on screen until reset restarts the loop.
      tl.add(() => {
        ttsActive = false;
        bandActive = false;
      });
      register({
        timeline: tl,
        reset: () => {
          reset();
          tl.pause(0);
          demo.blur();
          demo.placeFrac(0.5, 0.6);
        },
      });
    })();
    return () => {
      cancelled = true;
      timeline?.kill();
    };
  });
</script>

<div bind:this={stageEl} class="relative w-full h-full overflow-hidden flex flex-col">
  <div class="flex-1 min-h-0 flex items-center justify-center">
    <div bind:this={columnEl} class="w-[620px] flex flex-col" style:gap={bubbleGap(ui)}>
      {#if showUser}
        <MessageEnter enabled={!measuring}>
          <UserMessageView text={userText} />
        </MessageEnter>
      {/if}
      {#if showAnswer}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView
            kind="content"
            bgClass="bubble-agent"
            active={ttsActive}
            pulse={ttsPulse}
          >
            {#snippet body()}
              <span class="whitespace-pre-wrap break-words">{agentText}</span>
            {/snippet}
          </AgentMessageView>
        </MessageEnter>
      {/if}
      <!-- Voice button is on by default (UserInputView reads stt.enabled from the
           context); these props only animate its scripted VAD state. -->
      <UserInputView
        bind:value={inputValue}
        {placeholder}
        {voiceClass}
        {vadEnabled}
        {vadListening}
        onVoiceToggle={noop}
      />
    </div>
  </div>

  <!-- Caption band: its own reserved row below the centered conversation, an
       overlay that represents spoken sound rather than a chat bubble, so it sits
       clear of the input and never shifts the conversation layout. -->
  <div class="shrink-0 h-20 flex items-center justify-center px-4">
    {#if showBand}
      <SubtitleBand speaker={bandSpeaker} text={bandText} active={bandActive} />
    {/if}
  </div>

  <Cursor bind:ref={cursorRef} />
</div>
