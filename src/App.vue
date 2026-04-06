<script setup lang="ts">
import Pet from "./components/Pet.vue";
import AuthNotification from "./components/AuthNotification.vue";
import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import meowSound from "./assets/sounds/meow.mp3";

const petState = ref<"idle" | "busy" | "success" | "fail" | "sleep" | "waiting_auth">("idle");
const authActive = ref(false);
let unlistenStarted: UnlistenFn | null = null;
let unlistenFinished: UnlistenFn | null = null;
let unlistenAuth: UnlistenFn | null = null;

function playMeow() {
  const audio = new Audio(meowSound);
  audio.volume = 0.5;
  audio.play().catch(() => {});
}

async function onContextMenu(e: MouseEvent) {
  e.preventDefault();
  await invoke("show_context_menu");
}

function onClick() {
  playMeow();
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
      petState.value = "idle";
    }, 2000);
  });
  // Play meow when Claude Code requests auth
  unlistenAuth = await listen("claude_auth_request", () => {
    playMeow();
  });
});

onUnmounted(() => {
  unlistenStarted?.();
  unlistenFinished?.();
  unlistenAuth?.();
});
</script>

<template>
  <div class="app" @contextmenu="onContextMenu" @click="onClick">
    <Pet :state="petState" />
    <AuthNotification @auth-active="onAuthActive" />
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
