<script setup lang="ts">
import { ref, nextTick, onMounted, onUnmounted, computed } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { catLayout } from "../composables/useSessionCats";

const DIALOG_WIDTH = 460;
const DIALOG_HEIGHT = 260;

const visible = ref(false);
const link = ref("");
const hint = ref("");
const submitting = ref(false);
const errorMsg = ref<string | null>(null);
const linkEl = ref<HTMLInputElement | null>(null);
let unlisten: UnlistenFn | null = null;

const canSubmit = computed(
  () => (link.value.trim() !== "" || hint.value.trim() !== "") && !submitting.value,
);

const emit = defineEmits<{ slackReplyActive: [active: boolean] }>();

async function open() {
  if (visible.value) return;
  link.value = "";
  hint.value = "";
  errorMsg.value = null;
  const win = getCurrentWindow();
  try {
    await win.setSize(new LogicalSize(DIALOG_WIDTH, DIALOG_HEIGHT));
  } catch {}
  visible.value = true;
  emit("slackReplyActive", true);
  await nextTick();
  // Context menu close leaves keyboard focus with whatever app had it before.
  // Explicitly pull the Pawkit window foreground, then focus on next frame.
  try {
    await win.setFocus();
  } catch {}
  requestAnimationFrame(() => {
    linkEl.value?.focus();
    linkEl.value?.select();
  });
}

async function close() {
  if (!visible.value) return;
  visible.value = false;
  link.value = "";
  hint.value = "";
  errorMsg.value = null;
  submitting.value = false;
  const { windowWidth, windowHeight } = catLayout.value;
  try {
    await getCurrentWindow().setSize(new LogicalSize(windowWidth, windowHeight));
  } catch {}
  emit("slackReplyActive", false);
}

async function submit() {
  if (!canSubmit.value) return;
  const payload = {
    link: link.value.trim() || null,
    hint: hint.value.trim() || null,
  };
  submitting.value = true;
  errorMsg.value = null;
  try {
    await invoke("slack_quick_reply", payload);
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
  unlisten = await listen("slack_quick_reply_prompt", () => open());
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <Transition name="fade-slack">
    <div v-if="visible" class="slack-backdrop" @mousedown.prevent>
      <div class="slack-card" @mousedown.stop @keydown="onKey">
        <div class="slack-title">Slack 快捷回复</div>

        <label class="slack-label">Slack 消息链接（可选）</label>
        <input
          ref="linkEl"
          v-model="link"
          class="slack-input"
          type="text"
          placeholder="https://slack.com/archives/Cxxx/pYYY"
          autocomplete="off"
          spellcheck="false"
          :disabled="submitting"
          @keydown.enter.exact="submit"
        />

        <label class="slack-label">附加提示词（可选）</label>
        <textarea
          v-model="hint"
          class="slack-textarea"
          rows="3"
          placeholder="补充回复意图、语气、关键信息..."
          :disabled="submitting"
          @keydown.enter.ctrl="submit"
          @keydown.enter.meta="submit"
        ></textarea>

        <div v-if="errorMsg" class="slack-error">{{ errorMsg }}</div>

        <div class="slack-buttons">
          <button class="slack-btn slack-btn-cancel" :disabled="submitting" @click="close">
            取消
          </button>
          <button
            class="slack-btn slack-btn-ok"
            :disabled="!canSubmit"
            @click="submit"
          >
            {{ submitting ? "启动中..." : "提交" }}
          </button>
        </div>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.slack-backdrop {
  position: fixed;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.4);
  z-index: 1000;
}

.slack-card {
  width: 420px;
  background: rgba(25, 30, 45, 0.97);
  border: 1px solid rgba(120, 100, 220, 0.45);
  border-radius: 8px;
  padding: 12px 14px;
  color: #e0e0e0;
  font-family: "Segoe UI", system-ui, sans-serif;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.6);
}

.slack-title {
  font-size: 12px;
  font-weight: 600;
  color: #c5b4ff;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 10px;
}

.slack-label {
  display: block;
  font-size: 10px;
  color: #9aa3b2;
  margin-bottom: 3px;
}

.slack-input,
.slack-textarea {
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
  margin-bottom: 8px;
  resize: none;
}

.slack-textarea {
  font-family: "Segoe UI", system-ui, sans-serif;
  line-height: 1.35;
}

.slack-input:focus,
.slack-textarea:focus {
  border-color: #8b7ee0;
}

.slack-error {
  color: #ff6b6b;
  font-size: 11px;
  margin: -2px 0 8px;
}

.slack-buttons {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}

.slack-btn {
  padding: 5px 14px;
  border: none;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s;
}

.slack-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.slack-btn-ok {
  background: #8b7ee0;
  color: #fff;
}

.slack-btn-cancel {
  background: #555;
  color: #ccc;
}

.slack-btn:not(:disabled):hover {
  opacity: 0.85;
}

.fade-slack-enter-active,
.fade-slack-leave-active {
  transition: opacity 0.15s ease;
}

.fade-slack-enter-from,
.fade-slack-leave-to {
  opacity: 0;
}
</style>
