<script setup lang="ts">
import Pet from "./components/Pet.vue";
import ContextMenu from "./components/ContextMenu.vue";
import { useActions } from "./composables/useActions";
import { ref } from "vue";

const { actions, runAction } = useActions();
const menuVisible = ref(false);
const menuX = ref(0);
const menuY = ref(0);
const petState = ref<"idle" | "busy" | "success" | "fail" | "sleep">("idle");

function onContextMenu(e: MouseEvent) {
  e.preventDefault();
  menuX.value = e.clientX;
  menuY.value = e.clientY;
  menuVisible.value = true;
}

function closeMenu() {
  menuVisible.value = false;
}

async function onActionSelect(actionId: string) {
  menuVisible.value = false;
  petState.value = "busy";
  try {
    const result = await runAction(actionId);
    petState.value = result.success ? "success" : "fail";
  } catch {
    petState.value = "fail";
  }
  setTimeout(() => {
    petState.value = "idle";
  }, 2000);
}
</script>

<template>
  <div class="app" @contextmenu="onContextMenu" @click="closeMenu">
    <Pet :state="petState" />
    <ContextMenu
      v-if="menuVisible"
      :actions="actions"
      :x="menuX"
      :y="menuY"
      @select="onActionSelect"
      @close="closeMenu"
    />
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
