// Store-to-store port wiring, kept out of the individual store modules so it
// can't re-form the import cycles it exists to break. messagesState and
// streamingState mutually drive each other, as do sessionsState and
// streamingState; to keep the static import graph acyclic, the "back" edge of
// each pair is an injected port rather than a direct import:
//
//   - streaming -> messages  : direct import (incl. the one reactive read,
//                              hasActiveWork's $derived on hasActiveToolCall).
//   - messages  -> streaming : StreamControl port, injected here.
//   - sessions  -> streaming : direct import (load/new/delete drive it).
//   - streaming -> sessions  : SessionPort, injected here.
//
// This module imports all three stores, so its body runs only after each has
// fully initialized (no temporal-dead-zone trap that top-level wiring inside the
// cycle would hit). Imported for its side effect from the state barrel, like
// settings-effects.

import { messagesState } from "./messages.svelte";
import { sessionsState } from "./sessions.svelte";
import { streamingState } from "./streaming.svelte";

messagesState.setStreamControl(streamingState);
streamingState.setSessionPort(sessionsState);
