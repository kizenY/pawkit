<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";

const props = defineProps<{
  state: "idle" | "busy" | "success" | "fail" | "sleep" | "waiting_auth" | "away";
  showBell?: boolean;
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
  waiting_auth: "#ddaa00",
  away: "#8888cc",
};

function drawCat(ctx: CanvasRenderingContext2D, state: string, frame: number) {
  ctx.clearRect(0, 0, CANVAS_SIZE, CANVAS_SIZE);
  ctx.imageSmoothingEnabled = false;

  const s = SCALE;
  const color = stateColors[state] || stateColors.idle;

  // Body bounce for animation
  const isStill = state === "sleep" || state === "away";
  const bounce = isStill ? 0 : Math.sin(frame * 0.5) * s;
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
  if (state === "sleep" || state === "away") {
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

  if (state === "away") {
    // Hanging signboard from cat's neck
    // String from neck
    ctx.fillStyle = "#996633";
    drawPixelRect(ctx, 30 * s, baseY + 12 * s, 2 * s, 10 * s);

    // Signboard background
    const signX = 16 * s;
    const signY = baseY + 22 * s;
    const signW = 30 * s;
    const signH = 16 * s;

    // Board shadow
    ctx.fillStyle = "#7a5522";
    drawPixelRect(ctx, signX + 1 * s, signY + 1 * s, signW, signH);

    // Board
    ctx.fillStyle = "#c8946a";
    drawPixelRect(ctx, signX, signY, signW, signH);

    // Board border
    ctx.fillStyle = "#996633";
    drawPixelRect(ctx, signX, signY, signW, 2 * s); // top
    drawPixelRect(ctx, signX, signY + signH - 2 * s, signW, 2 * s); // bottom
    drawPixelRect(ctx, signX, signY, 2 * s, signH); // left
    drawPixelRect(ctx, signX + signW - 2 * s, signY, 2 * s, signH); // right

    // Text "外出" on the sign
    ctx.fillStyle = "#3d2200";
    ctx.font = `bold ${6 * s}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText("外出", signX + signW / 2, signY + signH / 2 + 1 * s);
    ctx.textAlign = "start";
    ctx.textBaseline = "alphabetic";

    // Small swing animation on the string
    const swing = Math.sin(frame * 0.3) * 1 * s;
    ctx.fillStyle = "#996633";
    drawPixelRect(ctx, 30 * s + swing, baseY + 18 * s, 2 * s, 4 * s);
  }

  if (state === "waiting_auth") {
    // Blinking question mark
    ctx.fillStyle = frame % 4 < 3 ? "#ffdd00" : "#ff8800";
    ctx.font = `bold ${12 * s}px monospace`;
    ctx.fillText("?", 48 * s, baseY - 6 * s);
  }
}

function drawBell(ctx: CanvasRenderingContext2D, state: string, frame: number) {
  const s = SCALE;
  const isStill = state === "sleep" || state === "away";
  const bounce = isStill ? 0 : Math.sin(frame * 0.5) * s;
  const baseY = 30 * s + bounce;

  // Bell hangs from cat's neck, slightly to the right
  const bellX = 32 * s;
  const bellY = baseY + 10 * s;

  // Gentle swing
  const swing = Math.sin(frame * 0.6) * 1.5 * s;

  // String
  ctx.fillStyle = "#aa8833";
  drawPixelRect(ctx, bellX + swing * 0.5, bellY, 1 * s, 4 * s);

  // Bell body (golden)
  ctx.fillStyle = "#ffcc00";
  drawPixelRect(ctx, bellX - 3 * s + swing, bellY + 4 * s, 7 * s, 5 * s);
  drawPixelRect(ctx, bellX - 2 * s + swing, bellY + 3 * s, 5 * s, 2 * s);

  // Bell rim (darker gold)
  ctx.fillStyle = "#dd9900";
  drawPixelRect(ctx, bellX - 3 * s + swing, bellY + 8 * s, 7 * s, 2 * s);

  // Bell clapper (dark dot)
  ctx.fillStyle = "#885500";
  drawPixelRect(ctx, bellX + swing, bellY + 9 * s, 1 * s, 2 * s);

  // Highlight (shine)
  ctx.fillStyle = "#ffee88";
  drawPixelRect(ctx, bellX - 1 * s + swing, bellY + 5 * s, 2 * s, 2 * s);
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
  if (props.showBell) {
    drawBell(ctx, props.state, animFrame);
  }
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
    :class="{ 'pet-canvas--auth': state === 'waiting_auth' }"
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
  transition: top 0.2s ease;
}

.pet-canvas--auth {
  top: 30%;
}

.pet-canvas:active {
  cursor: grabbing;
}
</style>
