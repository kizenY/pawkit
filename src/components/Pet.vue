<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";

const props = defineProps<{
  state: "idle" | "busy" | "success" | "fail" | "sleep";
}>();

const canvas = ref<HTMLCanvasElement | null>(null);
const isDragging = ref(false);

// Pixel cat sprite - drawn procedurally since we don't have sprite files yet
const FRAME_SIZE = 64;
const SCALE = 2;
const CANVAS_SIZE = FRAME_SIZE * SCALE;

let animFrame = 0;
let frameCounter = 0;
let animTimer: number | null = null;

const stateColors: Record<string, string> = {
  idle: "#666666",
  busy: "#ff8800",
  success: "#44bb44",
  fail: "#dd4444",
  sleep: "#8888cc",
};

function drawCat(ctx: CanvasRenderingContext2D, state: string, frame: number) {
  ctx.clearRect(0, 0, CANVAS_SIZE, CANVAS_SIZE);
  ctx.imageSmoothingEnabled = false;

  const s = SCALE;
  const color = stateColors[state] || stateColors.idle;

  // Body bounce for animation
  const bounce = state === "sleep" ? 0 : Math.sin(frame * 0.5) * s;
  const baseY = 30 * s + bounce;

  ctx.fillStyle = color;

  // Ears
  drawPixelRect(ctx, 18 * s, baseY - 16 * s, 6 * s, 8 * s);
  drawPixelRect(ctx, 38 * s, baseY - 16 * s, 6 * s, 8 * s);

  // Head
  drawPixelRect(ctx, 14 * s, baseY - 10 * s, 34 * s, 20 * s);

  // Body
  drawPixelRect(ctx, 16 * s, baseY + 10 * s, 30 * s, 20 * s);

  // Legs
  const legAnim = state === "busy" ? Math.sin(frame * 1.5) * 3 * s : 0;
  drawPixelRect(ctx, 18 * s, baseY + 30 * s + legAnim, 8 * s, 10 * s);
  drawPixelRect(ctx, 36 * s, baseY + 30 * s - legAnim, 8 * s, 10 * s);

  // Tail
  const tailWag = Math.sin(frame * 0.8) * 5 * s;
  drawPixelRect(ctx, 46 * s, baseY + 14 * s + tailWag, 6 * s, 4 * s);
  drawPixelRect(ctx, 50 * s, baseY + 10 * s + tailWag, 4 * s, 8 * s);

  // Eyes
  ctx.fillStyle = "#ffffff";
  if (state === "sleep") {
    // Closed eyes (lines)
    ctx.fillStyle = "#333333";
    drawPixelRect(ctx, 22 * s, baseY - 2 * s, 6 * s, 2 * s);
    drawPixelRect(ctx, 34 * s, baseY - 2 * s, 6 * s, 2 * s);
  } else {
    // Open eyes
    drawPixelRect(ctx, 22 * s, baseY - 6 * s, 6 * s, 6 * s);
    drawPixelRect(ctx, 34 * s, baseY - 6 * s, 6 * s, 6 * s);
    // Pupils
    ctx.fillStyle = "#333333";
    const pupilOffset = state === "busy" ? Math.sin(frame * 2) * s : 0;
    drawPixelRect(ctx, 24 * s + pupilOffset, baseY - 4 * s, 3 * s, 3 * s);
    drawPixelRect(ctx, 36 * s + pupilOffset, baseY - 4 * s, 3 * s, 3 * s);
  }

  // Mouth
  ctx.fillStyle = "#333333";
  if (state === "success") {
    // Smile
    drawPixelRect(ctx, 26 * s, baseY + 4 * s, 10 * s, 2 * s);
    drawPixelRect(ctx, 24 * s, baseY + 2 * s, 2 * s, 2 * s);
    drawPixelRect(ctx, 36 * s, baseY + 2 * s, 2 * s, 2 * s);
  } else if (state === "fail") {
    // Frown
    drawPixelRect(ctx, 26 * s, baseY + 2 * s, 10 * s, 2 * s);
    drawPixelRect(ctx, 24 * s, baseY + 4 * s, 2 * s, 2 * s);
    drawPixelRect(ctx, 36 * s, baseY + 4 * s, 2 * s, 2 * s);
  } else {
    // Neutral
    drawPixelRect(ctx, 28 * s, baseY + 2 * s, 6 * s, 2 * s);
  }

  // State-specific effects
  if (state === "busy") {
    // Sweat drops
    ctx.fillStyle = "#88ccff";
    drawPixelRect(ctx, 50 * s, baseY - 8 * s + (frame % 4) * 2 * s, 3 * s, 3 * s);
  }

  if (state === "sleep") {
    // ZZZ
    ctx.fillStyle = "#aaaacc";
    const zOffset = (frame % 6) * s;
    ctx.font = `${10 * s}px monospace`;
    ctx.fillText("z", 48 * s, baseY - 4 * s - zOffset);
    ctx.font = `${8 * s}px monospace`;
    ctx.fillText("z", 52 * s, baseY - 12 * s - zOffset);
  }
}

function drawPixelRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number
) {
  ctx.fillRect(Math.round(x), Math.round(y), Math.round(w), Math.round(h));
}

function animate() {
  if (!canvas.value) return;
  const ctx = canvas.value.getContext("2d");
  if (!ctx) return;

  frameCounter++;
  if (frameCounter >= 8) {
    frameCounter = 0;
    animFrame++;
  }

  drawCat(ctx, props.state, animFrame);
  animTimer = requestAnimationFrame(animate);
}

// Drag support
async function onMouseDown(e: MouseEvent) {
  if (e.button !== 0) return;
  isDragging.value = true;
  const appWindow = getCurrentWindow();
  await appWindow.startDragging();
  isDragging.value = false;
}

watch(
  () => props.state,
  () => {
    animFrame = 0;
  }
);

onMounted(() => {
  animate();
});

onUnmounted(() => {
  if (animTimer !== null) {
    cancelAnimationFrame(animTimer);
  }
});
</script>

<template>
  <canvas
    ref="canvas"
    :width="CANVAS_SIZE"
    :height="CANVAS_SIZE"
    class="pet-canvas"
    @mousedown="onMouseDown"
  />
</template>

<style scoped>
.pet-canvas {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  cursor: grab;
  image-rendering: pixelated;
}

.pet-canvas:active {
  cursor: grabbing;
}
</style>
