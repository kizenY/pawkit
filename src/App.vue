<script setup lang="ts">
import Pet from "./components/Pet.vue";
import AuthNotification from "./components/AuthNotification.vue";
import ReviewNotification from "./components/ReviewNotification.vue";
import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import meowSound from "./assets/sounds/meow.mp3";
import bellSound from "./assets/sounds/bell.mp3";

const petState = ref<"idle" | "busy" | "success" | "fail" | "sleep" | "waiting_auth" | "away" | "knock">("idle");
const authActive = ref(false);
const isAway = ref(false);
const hasUnread = ref(false);
const reviewBubble = ref<"reviewing" | "done" | null>(null);
let unlistenStarted: UnlistenFn | null = null;
let unlistenFinished: UnlistenFn | null = null;
let unlistenAuth: UnlistenFn | null = null;
let unlistenMode: UnlistenFn | null = null;
let unlistenTaskDone: UnlistenFn | null = null;
let unlistenKnock: UnlistenFn | null = null;
let unlistenTerminalFocused: UnlistenFn | null = null;
let unlistenClaudeActive: UnlistenFn | null = null;
let claudeIdleTimer: ReturnType<typeof setTimeout> | null = null;

// Pre-create Audio objects to avoid autoplay policy issues.
// Webview may block new Audio().play() from non-user-interaction contexts.
// By reusing the same Audio objects, once unlocked by a click they stay unlocked.
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

async function onContextMenu(e: MouseEvent) {
  e.preventDefault();
  await invoke("show_context_menu");
}

function onClick() {
  if (hasUnread.value || reviewBubble.value || petState.value === "success" || petState.value === "knock") {
    hasUnread.value = false;
    reviewBubble.value = null;
    petState.value = "idle";
  }
  // Terminal focus is handled by Pet.vue (only on click, not drag)
}

function onReviewActive(active: boolean) {
  if (active && petState.value !== "waiting_auth") {
    petState.value = "waiting_auth";
  } else if (!active && petState.value === "waiting_auth" && !authActive.value) {
    petState.value = "idle";
  }
}

function onAuthActive(active: boolean) {
  authActive.value = active;
  if (active) {
    petState.value = "waiting_auth";
  } else if (petState.value === "waiting_auth") {
    petState.value = "idle";
  }
}

onMounted(async () => {
  unlistenStarted = await listen("action_started", () => {
    petState.value = "busy";
    playMeow();
  });
  unlistenFinished = await listen<{ success: boolean }>("action_finished", (event) => {
    petState.value = event.payload.success ? "success" : "fail";
    setTimeout(() => {
      if (isAway.value) {
        petState.value = "away";
      } else {
        petState.value = "idle";
      }
    }, 2000);
  });
  // Play meow when Claude Code requests auth (only in home mode)
  unlistenAuth = await listen("claude_auth_request", () => {
    if (!isAway.value) {
      playMeow();
      // New state clears the review "done" lightbulb
      if (reviewBubble.value === "done") reviewBubble.value = null;
    }
  });
  // Mode change: away / home
  unlistenMode = await listen<string>("mode_changed", (event) => {
    const mode = event.payload;
    isAway.value = mode === "away";
    if (mode === "away") {
      petState.value = "away";
      hasUnread.value = false;
    } else {
      petState.value = "idle";
    }
  });
  // Claude Code task completed — show bell as unread indicator (skip in away mode)
  unlistenTaskDone = await listen("claude_task_done", () => {
    if (isAway.value) return;
    // Clear busy state
    if (claudeIdleTimer) clearTimeout(claudeIdleTimer);
    // New state clears the review "done" lightbulb
    if (reviewBubble.value === "done") reviewBubble.value = null;
    // Enter success state with bell — stays until user clicks
    petState.value = "success";
    hasUnread.value = true;
    playBell();
  });
  // Claude Code notification (permission, idle, auth) — knock knock
  unlistenKnock = await listen("claude_knock", () => {
    if (isAway.value) return;
    if (claudeIdleTimer) clearTimeout(claudeIdleTimer);
    petState.value = "knock";
    hasUnread.value = true;
    playBell();
  });
  // User switched to Claude terminal directly — clear unread
  unlistenTerminalFocused = await listen("terminal_focused", () => {
    hasUnread.value = false;
  });
  // Claude Code is actively working (PreToolUse hook fired) — show busy
  unlistenClaudeActive = await listen("claude_active", () => {
    if (isAway.value) return;
    if (petState.value !== "waiting_auth") {
      petState.value = "busy";
    }
    // Reset idle timer — go back to idle after 15s of no activity
    if (claudeIdleTimer) clearTimeout(claudeIdleTimer);
    claudeIdleTimer = setTimeout(() => {
      if (petState.value === "busy") {
        petState.value = "idle";
      }
    }, 15000);
  });
});

onUnmounted(() => {
  unlistenStarted?.();
  unlistenFinished?.();
  unlistenAuth?.();
  unlistenMode?.();
  unlistenTaskDone?.();
  unlistenKnock?.();
  unlistenTerminalFocused?.();
  unlistenClaudeActive?.();
});
</script>

<template>
  <div class="app" @contextmenu="onContextMenu">
    <Pet :state="petState" :show-bell="hasUnread" :review-bubble="reviewBubble" @pet-click="onClick" />
    <AuthNotification v-if="!isAway" @auth-active="onAuthActive" />
    <ReviewNotification v-if="!isAway && !authActive" @review-active="onReviewActive" @review-bubble="(s: any) => reviewBubble = s" />
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
</style>
