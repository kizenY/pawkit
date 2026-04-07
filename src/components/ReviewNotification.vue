<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

interface ReviewItem {
  id: string;
  repo: string;
  pr_number: number;
  title: string;
  url: string;
  item_type: string;
  body: string;
}

const queue = ref<ReviewItem[]>([]);
const current = ref<ReviewItem | null>(null);
const processing = ref(false);
let unlistenFound: UnlistenFn | null = null;
let unlistenDone: UnlistenFn | null = null;
let unlistenError: UnlistenFn | null = null;

const emit = defineEmits<{
  reviewActive: [active: boolean];
  reviewBubble: [state: "reviewing" | "done" | null];
}>();

function processQueue() {
  if (current.value || processing.value) return;
  if (queue.value.length === 0) {
    emit("reviewActive", false);
    return;
  }
  current.value = queue.value.shift()!;
  emit("reviewActive", true);
}

async function approve() {
  if (!current.value) return;
  processing.value = true;
  emit("reviewBubble", "reviewing");
  await invoke("approve_review_item", { id: current.value.id });
  current.value = null;
  // Don't clear processing here — wait for review_item_done/error event
}

function openPr() {
  if (!current.value) return;
  const url = current.value.url || `https://github.com/${current.value.repo}/pull/${current.value.pr_number}`;
  openUrl(url);
}

function skip() {
  if (!current.value) return;
  invoke("skip_review_item", { id: current.value.id });
  current.value = null;
  processQueue();
}

onMounted(async () => {
  unlistenFound = await listen<ReviewItem>("review_item_found", (event) => {
    queue.value.push(event.payload);
    processQueue();
  });
  unlistenDone = await listen<string>("review_item_done", () => {
    processing.value = false;
    emit("reviewBubble", "done");
    processQueue();
  });
  unlistenError = await listen("review_item_error", () => {
    processing.value = false;
    emit("reviewBubble", "done");
    processQueue();
  });
});

onUnmounted(() => {
  unlistenFound?.();
  unlistenDone?.();
  unlistenError?.();
});
</script>

<template>
  <Transition name="slide-review">
    <div v-if="current && !processing" class="review-card" @mousedown.prevent>
      <div class="review-type">
        {{ current.item_type === "review_request" ? "Review" : current.item_type === "comment" ? "Comment" : "@Mention" }}
      </div>
      <div class="review-title" @mousedown.prevent @click="openPr">
        {{ current.repo.split("/")[1] }}#{{ current.pr_number }}
      </div>
      <div class="review-desc">{{ current.title }}</div>
      <div class="review-buttons">
        <button class="btn-skip" @mousedown.prevent @click="skip">Skip</button>
        <button class="btn-review" @mousedown.prevent @click="approve">Handle</button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.review-card {
  position: absolute;
  bottom: 4px;
  left: 50%;
  transform: translateX(-50%);
  width: 170px;
  background: rgba(25, 30, 45, 0.95);
  border: 1px solid rgba(80, 160, 255, 0.3);
  border-radius: 6px;
  padding: 6px;
  color: #e0e0e0;
  font-family: "Segoe UI", system-ui, sans-serif;
  font-size: 11px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
}

.review-type {
  font-size: 9px;
  font-weight: 600;
  color: #6ab0ff;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 2px;
}

.review-title {
  font-weight: 700;
  font-size: 11px;
  color: #6ab0ff;
  margin-bottom: 2px;
  cursor: pointer;
}
.review-title:hover {
  text-decoration: underline;
}

.review-desc {
  font-size: 9px;
  color: #aaaaaa;
  word-break: break-all;
  max-height: 28px;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-bottom: 4px;
  line-height: 1.2;
}

.review-buttons {
  display: flex;
  gap: 4px;
}

.review-buttons button {
  flex: 1;
  padding: 4px 0;
  border: none;
  border-radius: 4px;
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  transition: opacity 0.15s;
}

.review-buttons button:hover {
  opacity: 0.85;
}

.btn-review {
  background: #3b82f6;
  color: #fff;
}

.btn-skip {
  background: #555;
  color: #ccc;
}

.slide-review-enter-active {
  transition: all 0.2s ease-out;
}
.slide-review-leave-active {
  transition: all 0.15s ease-in;
}
.slide-review-enter-from {
  opacity: 0;
  transform: translateX(-50%) translateY(8px);
}
.slide-review-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(8px);
}
</style>
