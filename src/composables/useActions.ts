import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Action {
  id: string;
  name: string;
  icon?: string;
  action_type: string;
  group?: string;
  confirm: boolean;
  enabled: boolean;
}

export interface ActionResult {
  action_id: string;
  success: boolean;
  stdout: string;
  stderr: string;
  exit_code: number | null;
  duration_ms: number;
}

export function useActions() {
  const actions = ref<Action[]>([]);
  let unlisten: UnlistenFn | null = null;

  async function reload() {
    try {
      actions.value = await invoke<Action[]>("get_actions");
    } catch (e) {
      console.error("Failed to load actions:", e);
    }
  }

  async function runAction(actionId: string): Promise<ActionResult> {
    return invoke<ActionResult>("run_action", { actionId });
  }

  onMounted(async () => {
    await reload();
    unlisten = await listen("config_changed", () => {
      reload();
    });
  });

  onUnmounted(() => {
    unlisten?.();
  });

  return { actions, runAction, reload };
}
