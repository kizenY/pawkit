import { reactive, computed, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";

export type PetState =
  | "idle"
  | "busy"
  | "success"
  | "fail"
  | "sleep"
  | "waiting_auth"
  | "away"
  | "knock";

/** Base state: persistent, controlled by UserPrompt/Stop/ModeChange */
export type BaseState = "idle" | "busy" | "away";

/** Temporary state: overlays base state, auto-clears after timeout or user action */
export type TempState = "success" | "fail" | "waiting_auth" | "knock" | null;

/** Resolve display state: temp state takes priority over base state */
function resolveState(base: BaseState, temp: TempState): PetState {
  return temp ?? base;
}

export interface CatSession {
  sessionId: string;
  title: string;
  workingDir: string;
  baseState: BaseState;
  tempState: TempState;
  hasUnread: boolean;
  reviewBubble: "reviewing" | "done" | null;
}

/** Computed display state for a cat session */
export function catDisplayState(cat: CatSession): PetState {
  return resolveState(cat.baseState, cat.tempState);
}

const sessions = reactive(new Map<string, CatSession>());
const isAway = ref(false);
const isGreenLight = ref(false);

// Compute cat display properties based on session count
export const catLayout = computed(() => {
  const count = sessions.size;
  if (count === 0) {
    return { spriteSize: 102, slotWidth: 200, windowWidth: 200, windowHeight: 200 };
  }
  if (count === 1) {
    return { spriteSize: 80, slotWidth: 180, windowWidth: 200, windowHeight: 200 };
  }
  // Dynamic scaling: sprite shrinks gradually, minimum 40px
  const spriteSize = Math.max(40, Math.round(88 - count * 8));
  const gap = 8;
  // Each slot needs space for sprite + title label + overlays
  const slotWidth = Math.max(spriteSize + 40, 140);
  const windowWidth = Math.min(1200, count * slotWidth + (count - 1) * gap + 32);
  return { spriteSize, slotWidth, windowWidth, windowHeight: 200 };
});

// Idle cat state (when no sessions)
const idleCatBaseState = ref<BaseState>("idle");
const idleCatTempState = ref<TempState>(null);
const idleCatState = computed<PetState>(() =>
  resolveState(idleCatBaseState.value, idleCatTempState.value)
);
const idleCatUnread = ref(false);
const idleCatReviewBubble = ref<"reviewing" | "done" | null>(null);
const unlisteners: UnlistenFn[] = [];

function getOrCreateSession(sessionId: string, title?: string): CatSession {
  if (!sessions.has(sessionId)) {
    sessions.set(sessionId, {
      sessionId,
      title: title || `Session ${sessionId.slice(0, 8)}`,
      workingDir: "",
      baseState: isAway.value ? "away" : "idle",
      tempState: null,
      hasUnread: false,
      reviewBubble: null,
    });
    resizeWindow();
  }
  return sessions.get(sessionId)!;
}

function removeSession(sessionId: string) {
  sessions.delete(sessionId);
  resizeWindow();
}

async function resizeWindow() {
  const layout = catLayout.value;
  try {
    await getCurrentWindow().setSize(
      new LogicalSize(layout.windowWidth, layout.windowHeight)
    );
  } catch {
    // Ignore resize errors
  }
}

export async function initSessionCats() {
  // Session discovered — new cat appears
  unlisteners.push(
    await listen<{ session_id: string; title: string; working_dir: string }>(
      "session_discovered",
      (event) => {
        const { session_id, title, working_dir } = event.payload;
        const cat = getOrCreateSession(session_id, title);
        cat.title = title;
        cat.workingDir = working_dir;
      }
    )
  );

  // Session title updated (LLM-refined title)
  unlisteners.push(
    await listen<{ session_id: string; title: string }>("session_title_updated", (event) => {
      const cat = sessions.get(event.payload.session_id);
      if (cat) {
        cat.title = event.payload.title;
      }
    })
  );

  // Session ended — cat disappears
  unlisteners.push(
    await listen<{ session_id: string }>("session_ended", (event) => {
      removeSession(event.payload.session_id);
    })
  );

  // Claude active → base state = busy
  unlisteners.push(
    await listen<{ session_id?: string }>("claude_active", (event) => {
      if (isAway.value) return;
      const sid = event.payload?.session_id;
      if (sid && sessions.has(sid)) {
        const cat = sessions.get(sid)!;
        cat.baseState = "busy";
        // No idle timer — rely on claude_stopped event and backend liveness polling
      } else if (sessions.size === 0) {
        idleCatBaseState.value = "busy";
      }
    })
  );

  // Claude stopped → base state = idle (but stay "away" in away mode)
  unlisteners.push(
    await listen<{ session_id?: string }>("claude_stopped", (event) => {
      if (isAway.value) return;
      const sid = event.payload?.session_id;
      if (sid && sessions.has(sid)) {
        const cat = sessions.get(sid)!;
        cat.baseState = "idle";
      } else if (sessions.size === 0) {
        idleCatBaseState.value = "idle";
      }
    })
  );

  // Claude task done → temp state = success (with bell)
  unlisteners.push(
    await listen<{ session_id?: string }>("claude_task_done", (event) => {
      if (isAway.value) return;
      const sid = event.payload?.session_id;
      if (sid && sessions.has(sid)) {
        const cat = sessions.get(sid)!;
        if (cat.reviewBubble === "done") cat.reviewBubble = null;
        cat.tempState = "success";
        cat.hasUnread = true;
      } else if (sessions.size === 0) {
        if (idleCatReviewBubble.value === "done") idleCatReviewBubble.value = null;
        idleCatTempState.value = "success";
        idleCatUnread.value = true;
      }
    })
  );

  // Claude knock → temp state = knock (with bell)
  unlisteners.push(
    await listen<{ session_id?: string }>("claude_knock", (event) => {
      if (isAway.value) return;
      const sid = event.payload?.session_id;
      if (sid && sessions.has(sid)) {
        const cat = sessions.get(sid)!;
        cat.tempState = "knock";
        cat.hasUnread = true;
      } else if (sessions.size === 0) {
        idleCatTempState.value = "knock";
        idleCatUnread.value = true;
      }
    })
  );

  // Mode changes → base state (applies to ALL cats, not just idle)
  unlisteners.push(
    await listen<string>("mode_changed", (event) => {
      const mode = event.payload;
      isAway.value = mode === "away";
      if (mode === "away") {
        idleCatBaseState.value = "away";
        idleCatTempState.value = null;
        idleCatUnread.value = false;
        // Set ALL session cats to away
        for (const cat of sessions.values()) {
          cat.baseState = "away";
          cat.tempState = null;
        }
      } else {
        idleCatBaseState.value = "idle";
        idleCatTempState.value = null;
        // Restore all session cats to idle
        for (const cat of sessions.values()) {
          cat.baseState = "idle";
          cat.tempState = null;
        }
      }
    })
  );

  // Green light toggle
  unlisteners.push(
    await listen<boolean>("green_light_changed", (event) => {
      isGreenLight.value = event.payload;
    })
  );

  // Session stuck (future: could set a stuck visual indicator)
  unlisteners.push(
    await listen<{ session_id: string }>("session_stuck", (_event) => {
      // Reserved for future stuck visual state
    })
  );

  // Pull active sessions from backend — covers sessions discovered before listeners were ready
  // Pull twice: immediately (in case scan already ran) and after 3s (in case scan is still running)
  const pullActiveSessions = async () => {
    try {
      const existing = await invoke<Array<{ session_id: string; title: string; working_dir: string }>>(
        "get_active_sessions"
      );
      for (const s of existing) {
        if (!sessions.has(s.session_id)) {
          const cat = getOrCreateSession(s.session_id, s.title);
          cat.workingDir = s.working_dir;
        }
      }
    } catch {
      // Backend not ready yet
    }
  };
  await pullActiveSessions();
  setTimeout(pullActiveSessions, 3000);
}

export function cleanupSessionCats() {
  for (const unlisten of unlisteners) {
    unlisten();
  }
  unlisteners.length = 0;
  sessions.clear();
}

export function useSessionCats() {
  return {
    sessions,
    idleCatState,
    idleCatBaseState,
    idleCatTempState,
    idleCatUnread,
    idleCatReviewBubble,
    isAway,
    isGreenLight,
    catLayout,
    catDisplayState,

    /** Clear temp state on idle cat (click to dismiss) → returns to base state */
    clearIdleCat() {
      idleCatUnread.value = false;
      idleCatReviewBubble.value = null;
      idleCatTempState.value = null;
    },

    /** Clear temp state on a session cat (click to dismiss) → returns to base state */
    clearCat(sessionId: string) {
      const cat = sessions.get(sessionId);
      if (cat) {
        cat.hasUnread = false;
        cat.reviewBubble = null;
        cat.tempState = null;
      }
    },

    /** Set temp state on idle cat (for auth/review overlays) */
    setIdleCatTempState(state: TempState) {
      idleCatTempState.value = state;
    },
  };
}
