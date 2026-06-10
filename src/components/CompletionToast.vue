<script setup lang="ts">
import type { CompletionBadge } from '../types/agent'

const props = defineProps<{ badge: CompletionBadge }>()
const emit = defineEmits<{ dismiss: [sessionId: string] }>()

/** Truncate the CC completion message to keep the chip compact. */
function truncate(text: string | undefined, max = 72): string {
  if (!text)
    return ''
  const cleaned = text.replace(/\n/g, ' ').trim()
  return cleaned.length > max ? `${cleaned.slice(0, max)}…` : cleaned
}

function label(): string {
  return truncate(props.badge.summary) || truncate(props.badge.session_title) || ''
}
</script>

<template>
  <div class="completion-toast">
    <div class="toast-row">
      <span class="toast-icon">✅</span>
      <span class="toast-headline">完工啦！</span>
      <button
        class="toast-close"
        title="Dismiss"
        @click.stop="emit('dismiss', badge.session_id)"
      >
        ✕
      </button>
    </div>
    <div
      v-if="label()"
      class="toast-message"
    >
      {{ label() }}
    </div>
  </div>
</template>

<style scoped>
.completion-toast {
  box-sizing: border-box;
  width: 100%;
  padding: 9px 12px 8px;
  border-radius: 14px;
  border: 1px solid rgba(60, 140, 80, 0.22);
  background: rgba(240, 252, 244, 0.96);
  box-shadow: 0 10px 30px rgba(30, 80, 50, 0.18);
  backdrop-filter: blur(10px);
  color: #1a2e20;
  pointer-events: auto;
  -webkit-user-select: none;
  user-select: none;
}

.toast-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.toast-icon {
  font-size: 15px;
  line-height: 1;
  flex-shrink: 0;
}

.toast-headline {
  flex: 1;
  font-size: 14px;
  font-weight: 700;
  color: #1a7a3a;
  letter-spacing: 0.01em;
}

.toast-close {
  flex-shrink: 0;
  width: 22px;
  height: 22px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: 999px;
  background: rgba(30, 80, 50, 0.1);
  color: #3a6a4a;
  font-size: 11px;
  font-weight: 700;
  cursor: pointer;
  transition: background 0.12s ease, transform 0.06s ease;
  padding: 0;
  line-height: 1;
}

.toast-close:hover {
  background: rgba(30, 80, 50, 0.18);
}

.toast-close:active {
  transform: scale(0.9);
}

.toast-message {
  margin-top: 4px;
  font-size: 12px;
  line-height: 1.4;
  color: #3a5040;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
