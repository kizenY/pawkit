<script setup lang="ts">
import Pet from "./components/Pet.vue";
import AuthNotification from "./components/AuthNotification.vue";
import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import meowSound from "./assets/sounds/meow.mp3";
import bellSound from "./assets/sounds/bell.mp3";

const petState = ref<"idle" | "busy" | "success" | "fail" | "sleep" | "waiting_auth" | "away">("idle");
const authActive = ref(false);
const isAway = ref(false);
const hasUnread = ref(false);
let unlistenStarted: UnlistenFn | null = null;
let unlistenFinished: UnlistenFn | null = null;
let unlistenAuth: UnlistenFn | null = null;
let unlistenMode: UnlistenFn | null = null;
let unlistenTaskDone: UnlistenFn | null = null;
let unlistenTerminalFocused: UnlistenFn | null = null;

function playMeow() {
  const audio = new Audio(meowSound);
  audio.volume = 0.5;
  audio.play().catch(() => {});
}

function playBell() {
  const audio = new Audio(bellSound);
  audio.volume = 0.7;
  audio.play().catch(() => {});
}

async function onContextMenu(e: MouseEvent) {
  e.preventDefault();
  await invoke("show_context_menu");
}

async function onClick() {
  if (hasUnread.value) {
    // Has unread: focus Claude terminal and clear bell
    hasUnread.value = false;
    await invoke("focus_claude_terminal");
  } else {
    playMeow();
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
  // Claude Code task completed — show bell as unread indicator
  unlistenTaskDone = await listen("claude_task_done", () => {
    hasUnread.value = true;
    playBell();
  });
  // User switched to Claude terminal directly — clear unread
  unlistenTerminalFocused = await listen("terminal_focused", () => {
    hasUnread.value = false;
  });
});

onUnmounted(() => {
  unlistenStarted?.();
  unlistenFinished?.();
  unlistenAuth?.();
  unlistenMode?.();
  unlistenTaskDone?.();
  unlistenTerminalFocused?.();
});
</script>

<template>
  <div class="app" @contextmenu="onContextMenu" @click="onClick">
    <Pet :state="petState" :show-bell="hasUnread" />
    <AuthNotification v-if="!isAway" @auth-active="onAuthActive" />
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
