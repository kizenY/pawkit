<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface AuthRequest {
  request_id: string;
  tool_name: string;
  tool_input_summary: string;
}

const queue = ref<AuthRequest[]>([]);
const current = ref<AuthRequest | null>(null);
let unlisten: UnlistenFn | null = null;

const emit = defineEmits<{
  authActive: [active: boolean];
}>();

function processQueue() {
  if (current.value) return;
  if (queue.value.length === 0) {
    emit("authActive", false);
    return;
  }
  current.value = queue.value.shift()!;
  emit("authActive", true);
}

async function respond(allow: boolean) {
  if (!current.value) return;
  await invoke("respond_auth", {
    requestId: current.value.request_id,
    allow,
  });
  current.value = null;
  processQueue();
}

onMounted(async () => {
  unlisten = await listen<AuthRequest>("claude_auth_request", (event) => {
    queue.value.push(event.payload);
    processQueue();
  });
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <Transition name="slide">
    <div v-if="current" class="auth-card" @mousedown.prevent>
      <div class="auth-header">
        <span class="auth-title">{{ current.tool_name }}</span>
        <span v-if="queue.length > 0" class="auth-badge">+{{ queue.length }}</span>
      </div>
      <div v-if="current.tool_input_summary" class="auth-summary">
        {{ current.tool_input_summary }}
      </div>
      <div class="auth-buttons">
        <button class="btn-deny" @mousedown.prevent @click="respond(false)">Deny</button>
        <button class="btn-allow" @mousedown.prevent @click="respond(true)">Allow</button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.auth-card {
  position: absolute;
  bottom: 4px;
  left: 50%;
  transform: translateX(-50%);
  width: 170px;
  background: rgba(30, 30, 40, 0.95);
  border: 1px solid rgba(100, 100, 255, 0.3);
  border-radius: 6px;
  padding: 6px;
  color: #e0e0e0;
  font-family: "Segoe UI", system-ui, sans-serif;
  font-size: 11px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
}

.auth-header {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-bottom: 3px;
}

.auth-title {
  font-weight: 700;
  font-size: 12px;
  color: #ffffff;
}

.auth-badge {
  margin-left: auto;
  background: rgba(255, 140, 0, 0.8);
  color: #fff;
  font-size: 9px;
  padding: 1px 4px;
  border-radius: 6px;
  font-weight: 600;
}

.auth-summary {
  font-size: 9px;
  color: #aaaaaa;
  word-break: break-all;
  max-height: 32px;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-bottom: 4px;
  line-height: 1.2;
  background: rgba(0, 0, 0, 0.3);
  padding: 3px;
  border-radius: 3px;
  font-family: "Cascadia Code", "Consolas", monospace;
}

.auth-buttons {
  display: flex;
  gap: 4px;
}

.auth-buttons button {
  flex: 1;
  padding: 4px 0;
  border: none;
  border-radius: 4px;
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s;
}

.auth-buttons button:hover {
  opacity: 0.85;
}

.btn-allow {
  background: #44bb44;
  color: #fff;
}

.btn-deny {
  background: #555;
  color: #ccc;
}

.slide-enter-active {
  transition: all 0.2s ease-out;
}

.slide-leave-active {
  transition: all 0.15s ease-in;
}

.slide-enter-from {
  opacity: 0;
  transform: translateX(-50%) translateY(8px);
}

.slide-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(8px);
}
</style>
