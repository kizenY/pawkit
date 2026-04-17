<script setup lang="ts">
import { ref, nextTick, onMounted, onUnmounted } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { catLayout } from "../composables/useSessionCats";

const DIALOG_WIDTH = 420;
const DIALOG_HEIGHT = 150;

const visible = ref(false);
const url = ref("");
const submitting = ref(false);
const errorMsg = ref<string | null>(null);
const inputEl = ref<HTMLInputElement | null>(null);
let unlisten: UnlistenFn | null = null;

const emit = defineEmits<{ quickReviewActive: [active: boolean] }>();

async function open() {
  if (visible.value) return;
  url.value = "";
  errorMsg.value = null;
  const win = getCurrentWindow();
  try {
    await win.setSize(new LogicalSize(DIALOG_WIDTH, DIALOG_HEIGHT));
  } catch {}
  visible.value = true;
  emit("quickReviewActive", true);
  await nextTick();
  // The window loses keyboard focus when the tray/context menu closes.
  // Pull it back to foreground, then focus the input on the next frame.
  try {
    await win.setFocus();
  } catch {}
  requestAnimationFrame(() => {
    inputEl.value?.focus();
    inputEl.value?.select();
  });
}

async function close() {
  if (!visible.value) return;
  visible.value = false;
  url.value = "";
  errorMsg.value = null;
  submitting.value = false;
  const { windowWidth, windowHeight } = catLayout.value;
  try {
    await getCurrentWindow().setSize(new LogicalSize(windowWidth, windowHeight));
  } catch {}
  emit("quickReviewActive", false);
}

async function submit() {
  const trimmed = url.value.trim();
  if (!trimmed || submitting.value) return;
  submitting.value = true;
  errorMsg.value = null;
  try {
    await invoke("quick_review", { url: trimmed });
    await close();
  } catch (e) {
    submitting.value = false;
    errorMsg.value = `启动失败: ${e}`;
  }
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") close();
}

onMounted(async () => {
  unlisten = await listen("quick_review_prompt", () => open());
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <Transition name="slide-qr">
    <div v-if="visible" class="qr-backdrop" @mousedown.prevent>
      <div class="qr-card" @mousedown.stop>
        <div class="qr-title">快捷 Review</div>
        <input
          ref="inputEl"
          v-model="url"
          class="qr-input"
          type="text"
          placeholder="https://github.com/owner/repo/pull/123"
          autocomplete="off"
          spellcheck="false"
          :disabled="submitting"
          @keydown="onKey"
          @keydown.enter="submit"
        />
        <div v-if="errorMsg" class="qr-error">{{ errorMsg }}</div>
        <div class="qr-buttons">
          <button class="qr-btn qr-btn-cancel" :disabled="submitting" @click="close">
            取消
          </button>
          <button
            class="qr-btn qr-btn-ok"
            :disabled="!url.trim() || submitting"
            @click="submit"
          >
            {{ submitting ? "启动中..." : "开始 Review" }}
          </button>
        </div>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.qr-backdrop {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.35);
  z-index: 1000;
}

.qr-card {
  width: 380px;
  background: rgba(25, 30, 45, 0.97);
  border: 1px solid rgba(80, 160, 255, 0.4);
  border-radius: 8px;
  padding: 12px 14px;
  color: #e0e0e0;
  font-family: "Segoe UI", system-ui, sans-serif;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.6);
}

.qr-title {
  font-size: 12px;
  font-weight: 600;
  color: #6ab0ff;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 8px;
}

.qr-input {
  width: 100%;
  box-sizing: border-box;
  padding: 6px 8px;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid rgba(255, 255, 255, 0.15);
  border-radius: 4px;
  color: #ffffff;
  font-size: 12px;
  font-family: "Cascadia Code", "Consolas", monospace;
  outline: none;
  margin-bottom: 10px;
}

.qr-input:focus {
  border-color: #3b82f6;
}

.qr-error {
  color: #ff6b6b;
  font-size: 11px;
  margin: -4px 0 8px;
}

.qr-buttons {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}

.qr-btn {
  padding: 5px 14px;
  border: none;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s;
}

.qr-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.qr-btn-ok {
  background: #3b82f6;
  color: #fff;
}

.qr-btn-cancel {
  background: #555;
  color: #ccc;
}

.qr-btn:not(:disabled):hover {
  opacity: 0.85;
}

.slide-qr-enter-active,
.slide-qr-leave-active {
  transition: opacity 0.15s ease;
}

.slide-qr-enter-from,
.slide-qr-leave-to {
  opacity: 0;
}
</style>
