<script setup lang="ts">
import { computed } from "vue";

interface ActionItem {
  id: string;
  name: string;
  icon?: string;
  group?: string;
  confirm: boolean;
}

const props = defineProps<{
  actions: ActionItem[];
  x: number;
  y: number;
}>();

const emit = defineEmits<{
  select: [actionId: string];
  close: [];
}>();

interface GroupedActions {
  name: string | null;
  items: ActionItem[];
}

const grouped = computed<GroupedActions[]>(() => {
  const groups = new Map<string | null, ActionItem[]>();

  for (const action of props.actions) {
    const key = action.group || null;
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key)!.push(action);
  }

  const result: GroupedActions[] = [];
  // Ungrouped first
  if (groups.has(null)) {
    result.push({ name: null, items: groups.get(null)! });
    groups.delete(null);
  }
  for (const [name, items] of groups) {
    result.push({ name, items });
  }
  return result;
});

function handleSelect(action: ActionItem) {
  if (action.confirm) {
    if (!confirm(`Execute "${action.name}"?`)) {
      return;
    }
  }
  emit("select", action.id);
}

function handleClickOutside() {
  emit("close");
}
</script>

<template>
  <div class="menu-overlay" @click="handleClickOutside">
    <div
      class="context-menu"
      :style="{ left: x + 'px', top: y + 'px' }"
      @click.stop
    >
      <template v-for="(group, gi) in grouped" :key="gi">
        <div v-if="group.name" class="group-label">{{ group.name }}</div>
        <div
          v-for="action in group.items"
          :key="action.id"
          class="menu-item"
          @click="handleSelect(action)"
        >
          <span class="menu-icon">{{ action.icon || ">" }}</span>
          <span class="menu-name">{{ action.name }}</span>
        </div>
        <div
          v-if="gi < grouped.length - 1"
          class="menu-separator"
        />
      </template>
    </div>
  </div>
</template>

<style scoped>
.menu-overlay {
  position: fixed;
  top: 0;
  left: 0;
  width: 100vw;
  height: 100vh;
  z-index: 1000;
}

.context-menu {
  position: absolute;
  background: #1e1e2e;
  border: 1px solid #444;
  border-radius: 8px;
  padding: 4px 0;
  min-width: 180px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.6);
  font-family: "Segoe UI", system-ui, sans-serif;
  font-size: 13px;
  color: #cdd6f4;
}

.group-label {
  padding: 4px 12px 2px;
  font-size: 11px;
  color: #888;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.menu-item {
  padding: 6px 12px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 8px;
  transition: background 0.1s;
}

.menu-item:hover {
  background: #313244;
}

.menu-icon {
  width: 20px;
  text-align: center;
  flex-shrink: 0;
}

.menu-name {
  flex: 1;
}

.menu-separator {
  height: 1px;
  background: #333;
  margin: 4px 8px;
}
</style>
