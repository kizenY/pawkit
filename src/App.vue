<script setup lang="ts">
import Pet from "./components/Pet.vue";
import AuthNotification from "./components/AuthNotification.vue";
import ReviewNotification from "./components/ReviewNotification.vue";
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  useSessionCats,
  initSessionCats,
  cleanupSessionCats,
  catLayout,
  catDisplayState,
} from "./composables/useSessionCats";
import meowSound from "./assets/sounds/meow.mp3";
import bellSound from "./assets/sounds/bell.mp3";

const {
  sessions,
  idleCatState,
  idleCatBaseState,
  idleCatTempState,
  idleCatUnread,
  idleCatReviewBubble,
  isAway,
  isGreenLight,
  clearIdleCat,
  clearCat,
  setIdleCatTempState,
} = useSessionCats();

const authActive = ref(false);
let unlistenStarted: UnlistenFn | null = null;
let unlistenFinished: UnlistenFn | null = null;
let unlistenAuth: UnlistenFn | null = null;
let unlistenTerminalFocused: UnlistenFn | null = null;

// Pre-create Audio objects
const meowAudio = new Audio(meowSound);
meowAudio.volume = 0.5;
const bellAudio = new Audio(bellSound);
bellAudio.volume = 0.7;

function playMeow() {
  meowAudio.currentTime = 0;
  meowAudio.play().catch(() => {});
}

function playBell() {
  bellAudio.currentTime = 0;
  bellAudio.play().catch(() => {});
}

// Session cats as array for rendering
const sessionList = computed(() => Array.from(sessions.values()));
const hasActiveSessions = computed(() => sessions.size > 0);

async function onContextMenu(e: MouseEvent, sessionId?: string) {
  e.preventDefault();
  await invoke("show_context_menu", { sessionId: sessionId || null });
}

function onIdleClick() {
  clearIdleCat();
}

function onSessionClick(sessionId: string) {
  clearCat(sessionId);
}

function onReviewActive(active: boolean) {
  if (active) {
    setIdleCatTempState("waiting_auth");
  } else if (idleCatTempState.value === "waiting_auth" && !authActive.value) {
    setIdleCatTempState(null); // Return to base state
  }
}

function onAuthActive(active: boolean) {
  authActive.value = active;
  if (active) {
    setIdleCatTempState("waiting_auth");
  } else if (idleCatTempState.value === "waiting_auth") {
    setIdleCatTempState(null); // Return to base state
  }
}

onMounted(async () => {
  // Initialize the multi-cat session tracking
  await initSessionCats();

  // Action events — use temp state for success/fail, base state stays as-is
  unlistenStarted = await listen("action_started", () => {
    idleCatBaseState.value = "busy";
    playMeow();
  });
  unlistenFinished = await listen<{ success: boolean }>("action_finished", (event) => {
    idleCatTempState.value = event.payload.success ? "success" : "fail";
    setTimeout(() => {
      // Clear temp state → display reverts to base state (could still be busy or idle)
      if (idleCatTempState.value === "success" || idleCatTempState.value === "fail") {
        idleCatTempState.value = null;
      }
    }, 2000);
  });
  // Play sounds on auth requests
  unlistenAuth = await listen("claude_auth_request", () => {
    if (!isAway.value) playMeow();
  });
  // Play bell on task done / knock
  await listen("claude_task_done", () => {
    if (!isAway.value) playBell();
  });
  await listen("claude_knock", () => {
    if (!isAway.value) playBell();
  });
  // Terminal focus clears idle cat unread
  unlistenTerminalFocused = await listen("terminal_focused", () => {
    idleCatUnread.value = false;
  });
});

onUnmounted(() => {
  unlistenStarted?.();
  unlistenFinished?.();
  unlistenAuth?.();
  unlistenTerminalFocused?.();
  cleanupSessionCats();
});
</script>

<template>
  <div class="app" @contextmenu.prevent="(e: MouseEvent) => onContextMenu(e)">
    <!-- Multi-cat display: one cat per active session -->
    <div v-if="hasActiveSessions" class="cats-row">
      <div
        v-for="cat in sessionList"
        :key="cat.sessionId"
        class="cat-slot"
        :style="{ maxWidth: catLayout.slotWidth + 'px' }"
        @contextmenu.prevent.stop="(e: MouseEvent) => onContextMenu(e, cat.sessionId)"
      >
        <Pet
          :state="catDisplayState(cat)"
          :show-bell="cat.hasUnread"
          :review-bubble="cat.reviewBubble"
          :title="cat.title"
          :size="catLayout.spriteSize"
          :is-idle-cat="false"
          :show-green-dot="isGreenLight"
          :session-id="cat.sessionId"
          @pet-click="onSessionClick(cat.sessionId)"
        />
      </div>
    </div>

    <!-- Idle cat: shown when no active sessions -->
    <div v-else class="idle-cat">
      <Pet
        :state="idleCatState"
        :show-bell="idleCatUnread"
        :review-bubble="idleCatReviewBubble"
        :size="102"
        :is-idle-cat="true"
        :show-green-dot="isGreenLight"
        @pet-click="onIdleClick"
      />
    </div>

    <AuthNotification v-if="!isAway" @auth-active="onAuthActive" />
    <ReviewNotification v-if="!isAway && !authActive" @review-active="onReviewActive" @review-bubble="(s: any) => idleCatReviewBubble = s" />
  </div>
</template>

<style>
html,
body {
  margin: 0;
  padding: 0;
  background: transparent;
  overflow: hidden;
}

.app {
  width: 100vw;
  height: 100vh;
  background: transparent;
  user-select: none;
  cursor: default;
}

.cats-row {
  display: flex;
  align-items: center;
  justify-content: center;
  flex-wrap: nowrap;
  height: 100%;
  gap: 8px;
  padding: 4px 8px;
}

.cat-slot {
  display: flex;
  flex-direction: column;
  align-items: center;
}

.idle-cat {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
}
</style>
