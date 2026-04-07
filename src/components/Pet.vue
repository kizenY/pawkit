<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";

const emit = defineEmits<{ (e: "pet-click"): void }>();

const props = defineProps<{
  state: "idle" | "busy" | "success" | "fail" | "sleep" | "waiting_auth" | "away";
  showBell?: boolean;
  reviewBubble?: "reviewing" | "done" | null;
}>();

const isDragging = ref(false);

// Map states to sprites
const spriteMap: Record<string, string> = {
  idle: "/idle.gif",
  busy: "/busy.gif",
  success: "/happy.gif",
  fail: "/fail.gif",
  sleep: "/cat.png",
  waiting_auth: "/question.gif",
  away: "/cat.png",
};

// Force GIF restart on state change by appending cache-buster
const gifKey = ref(0);
const spriteSrc = computed(() => {
  const base = spriteMap[props.state] || spriteMap.idle;
  return `${base}?v=${gifKey.value}`;
});

watch(() => props.state, () => {
  gifKey.value++;
});

// Drag support — only start dragging after actual mouse movement
function onMouseDown(e: MouseEvent) {
  if (e.button !== 0) return;
  const startX = e.clientX;
  const startY = e.clientY;
  const threshold = 5;

  function onMove(me: MouseEvent) {
    if (Math.abs(me.clientX - startX) + Math.abs(me.clientY - startY) > threshold) {
      cleanup();
      isDragging.value = true;
      getCurrentWindow().startDragging().then(() => { isDragging.value = false; });
    }
  }

  function onUp() {
    cleanup();
    // No movement — it's a click
    emit("pet-click");
    if (props.state !== "waiting_auth" && props.reviewBubble !== "done") {
      setTimeout(() => { invoke("focus_claude_terminal"); }, 50);
    }
  }

  function cleanup() {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
  }

  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

// Bell animation
const bellFrame = ref(0);
let bellTimer: number | null = null;
onMounted(() => {
  bellTimer = window.setInterval(() => { bellFrame.value++; }, 150);
});
onUnmounted(() => {
  if (bellTimer !== null) clearInterval(bellTimer);
});

const bellSwing = computed(() => Math.sin(bellFrame.value * 0.6) * 15);

// Review bubble animation
const bubbleFrame = ref(0);
let bubbleTimer: number | null = null;
onMounted(() => {
  bubbleTimer = window.setInterval(() => { bubbleFrame.value++; }, 200);
});
onUnmounted(() => {
  if (bubbleTimer !== null) clearInterval(bubbleTimer);
});

const bubbleDots = computed(() => {
  const phase = bubbleFrame.value % 4;
  return ".".repeat(phase);
});

// Lightbulb glow
const bulbGlow = computed(() => 0.6 + Math.sin(bubbleFrame.value * 0.4) * 0.4);

</script>

<template>
  <div class="pet-container" :class="{ 'pet-container--auth': state === 'waiting_auth' }" @mousedown="onMouseDown">
    <!-- Base sprite GIF -->
    <img
      :key="gifKey"
      :src="spriteSrc"
      class="pet-sprite"
      draggable="false"
    />

    <!-- Away overlay: signboard -->
    <div v-if="state === 'away'" class="overlay overlay--away-sign">
      外出
    </div>

    <!-- Bell indicator -->
    <div v-if="showBell" class="overlay overlay--bell" :style="{ transform: `rotate(${bellSwing}deg)` }">
      🔔
    </div>

    <!-- Review bubble: animated dots -->
    <div v-if="reviewBubble === 'reviewing'" class="overlay overlay--bubble">
      <span class="bubble-dots">{{ bubbleDots }}</span>
    </div>

    <!-- Review done: lightbulb -->
    <div v-if="reviewBubble === 'done'" class="overlay overlay--bulb" :style="{ opacity: bulbGlow }">
      💡
    </div>
  </div>
</template>

<style scoped>
.pet-container {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  cursor: grab;
  transition: top 0.2s ease;
  /* Size to fit sprite + overlays */
  width: 128px;
  height: 128px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.pet-container--auth {
  top: 30%;
}

.pet-container:active {
  cursor: grabbing;
}

.pet-sprite {
  image-rendering: pixelated;
  width: 102px;
  height: 102px;
  object-fit: contain;
  pointer-events: none;
  user-select: none;
}

/* Overlays positioned relative to container */
.overlay {
  position: absolute;
  pointer-events: none;
  user-select: none;
}

.overlay--question {
  top: -8px;
  right: -4px;
  font-size: 24px;
  font-weight: bold;
  color: #ffdd00;
  text-shadow: 1px 1px 2px rgba(0, 0, 0, 0.8);
  animation: bounce-q 0.6s ease-in-out infinite;
}

@keyframes bounce-q {
  0%, 100% { transform: translateY(0); }
  50% { transform: translateY(-4px); }
}

.overlay--away-sign {
  bottom: 4px;
  left: 50%;
  transform: translateX(-50%);
  background: #c8946a;
  border: 2px solid #8B6914;
  color: #3d2200;
  font-size: 10px;
  font-weight: bold;
  padding: 2px 6px;
  border-radius: 2px;
  box-shadow: 1px 1px 0 #6B4E12;
}

.overlay--bell {
  top: -20px;
  right: -16px;
  font-size: 20px;
  opacity: 0.8;
  transform-origin: top center;
}

.overlay--bubble {
  top: -4px;
  right: -12px;
  background: rgba(50, 50, 70, 0.9);
  color: #88bbff;
  font-size: 12px;
  font-family: monospace;
  padding: 2px 6px;
  border-radius: 4px;
  min-width: 28px;
  text-align: center;
}

.bubble-dots {
  letter-spacing: 2px;
}

.overlay--bulb {
  top: -8px;
  right: -8px;
  font-size: 18px;
  transition: opacity 0.3s ease;
}
</style>
