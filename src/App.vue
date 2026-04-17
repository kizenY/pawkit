<script setup lang="ts">
import Pet from "./components/Pet.vue";
import AuthNotification from "./components/AuthNotification.vue";
import ReviewNotification from "./components/ReviewNotification.vue";
import QuickReviewInput from "./components/QuickReviewInput.vue";
import SlackQuickReply from "./components/SlackQuickReply.vue";
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
const quickReviewActive = ref(false);
const slackReplyActive = ref(false);
const checkPrToast = ref<string | null>(null);
let checkPrTimer: ReturnType<typeof setTimeout> | null = null;
let unlistenStarted: UnlistenFn | null = null;
let unlistenFinished: UnlistenFn | null = null;
let unlistenAuth: UnlistenFn | null = null;
let unlistenTerminalFocused: UnlistenFn | null = null;
let unlistenCheckPrStarted: UnlistenFn | null = null;
let unlistenCheckPrFinished: UnlistenFn | null = null;
let unlistenQuickLaunched: UnlistenFn | null = null;
let unlistenSlackReplyLaunched: UnlistenFn | null = null;
let unlistenSlackReplyDone: UnlistenFn | null = null;

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
  // Check PR (manual trigger) — show thinking bubble and notify if nothing new
  unlistenCheckPrStarted = await listen("check_pr_started", () => {
    if (checkPrTimer) {
      clearTimeout(checkPrTimer);
      checkPrTimer = null;
    }
    idleCatReviewBubble.value = "reviewing";
    checkPrToast.value = "正在检查 PR...";
    if (!isAway.value) playMeow();
  });
  unlistenCheckPrFinished = await listen<{ found: number; disabled?: boolean }>(
    "check_pr_finished",
    (event) => {
      idleCatReviewBubble.value = null;
      const found = event.payload?.found ?? 0;
      if (event.payload?.disabled) {
        checkPrToast.value = "Auto Review 未开启";
      } else if (found === 0) {
        checkPrToast.value = "暂无新的 PR";
      } else {
        // New PRs will render via review_item_found flow — clear launch toast too
        checkPrToast.value = null;
        if (checkPrTimer) {
          clearTimeout(checkPrTimer);
          checkPrTimer = null;
        }
        return;
      }
      if (checkPrTimer) clearTimeout(checkPrTimer);
      checkPrTimer = setTimeout(() => {
        checkPrToast.value = null;
        checkPrTimer = null;
      }, 3000);
    },
  );
  // Slack quick reply — immediate feedback + completion toast
  unlistenSlackReplyLaunched = await listen("slack_quick_reply_launched", () => {
    idleCatReviewBubble.value = "reviewing";
    checkPrToast.value = "正在回复 Slack...";
    if (!isAway.value) playMeow();
    if (checkPrTimer) clearTimeout(checkPrTimer);
    checkPrTimer = setTimeout(() => {
      checkPrToast.value = null;
      checkPrTimer = null;
    }, 4000);
  });
  unlistenSlackReplyDone = await listen<{ success: boolean; error?: string }>(
    "slack_quick_reply_done",
    (event) => {
      idleCatReviewBubble.value = null;
      checkPrToast.value = event.payload.success ? "Slack 回复完成" : `回复失败: ${event.payload.error || "未知错误"}`;
      if (checkPrTimer) clearTimeout(checkPrTimer);
      checkPrTimer = setTimeout(() => {
        checkPrToast.value = null;
        checkPrTimer = null;
      }, 4000);
    },
  );

  // Quick Review launched — immediate feedback while Claude spins up
  unlistenQuickLaunched = await listen("quick_review_launched", () => {
    idleCatReviewBubble.value = "reviewing";
    checkPrToast.value = "已启动 Review...";
    if (!isAway.value) playMeow();
    if (checkPrTimer) clearTimeout(checkPrTimer);
    checkPrTimer = setTimeout(() => {
      checkPrToast.value = null;
      checkPrTimer = null;
      // Bubble is cleared once the session cat appears via session_discovered,
      // but fall back to clearing it here after 10s in case Claude never starts.
      if (idleCatReviewBubble.value === "reviewing") {
        idleCatReviewBubble.value = null;
      }
    }, 10000);
  });
});

onUnmounted(() => {
  unlistenStarted?.();
  unlistenFinished?.();
  unlistenAuth?.();
  unlistenTerminalFocused?.();
  unlistenCheckPrStarted?.();
  unlistenCheckPrFinished?.();
  unlistenQuickLaunched?.();
  unlistenSlackReplyLaunched?.();
  unlistenSlackReplyDone?.();
  if (checkPrTimer) clearTimeout(checkPrTimer);
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

    <AuthNotification v-if="!isAway && !quickReviewActive && !slackReplyActive" @auth-active="onAuthActive" />
    <ReviewNotification v-if="!isAway && !authActive && !quickReviewActive && !slackReplyActive" @review-active="onReviewActive" @review-bubble="(s: any) => idleCatReviewBubble = s" />

    <!-- Check PR feedback toast -->
    <Transition name="fade-toast">
      <div v-if="checkPrToast" class="check-pr-toast">{{ checkPrToast }}</div>
    </Transition>

    <QuickReviewInput @quick-review-active="quickReviewActive = $event" />
    <SlackQuickReply @slack-reply-active="slackReplyActive = $event" />
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

.check-pr-toast {
  position: absolute;
  bottom: 6px;
  left: 50%;
  transform: translateX(-50%);
  padding: 5px 10px;
  background: rgba(25, 30, 45, 0.95);
  border: 1px solid rgba(80, 160, 255, 0.35);
  border-radius: 6px;
  color: #d6e6ff;
  font-family: "Segoe UI", system-ui, sans-serif;
  font-size: 11px;
  white-space: nowrap;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  pointer-events: none;
}

.fade-toast-enter-active,
.fade-toast-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}
.fade-toast-enter-from,
.fade-toast-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(6px);
}
</style>
