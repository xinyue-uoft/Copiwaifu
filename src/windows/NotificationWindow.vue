<script setup lang="ts">
import type { UnlistenFn } from '@tauri-apps/api/event'
import type { AppBootstrap, NotificationCard, NotificationPayload } from '../types/agent'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log'
import { onMounted, onUnmounted, ref } from 'vue'

// Provided by App.vue for parity with the other windows; rendered in English.
defineProps<{ bootstrap: AppBootstrap }>()

// Passive notifications: one card per attention request — a permission
// approval or a choice an AI tool is waiting on. We make NO decision; the user
// resolves it in their terminal / AI tool. Cards auto-dissolve when the
// request resolves; Dismiss kills exactly this (session, epoch) instance and
// the window hides when none remain.
const cards = ref<NotificationCard[]>([])
let unlisten: UnlistenFn | null = null

function agentLabel(agent: string) {
  return agent.replace(/-/g, ' ')
}

function kindTag(card: NotificationCard) {
  return card.kind === 'choice' ? 'choose' : 'approval'
}

function kindHint(card: NotificationCard) {
  return card.kind === 'choice'
    ? 'Pick an option in your terminal / AI tool.'
    : 'Resolve it in your terminal / AI tool.'
}

function folderName(path?: string) {
  if (!path)
    return ''
  const parts = path.replace(/\/+$/, '').split('/')
  return parts[parts.length - 1] || path
}

function shortSession(id: string) {
  return id.length > 6 ? `…${id.slice(-6)}` : id
}

async function refresh() {
  try {
    const payload = await invoke<NotificationPayload>('get_notifications')
    cards.value = payload.cards
  }
  catch (error) {
    console.warn('failed to fetch notifications', error)
  }
}

async function dismiss(card: NotificationCard) {
  try {
    void logInfo(`[notif-ui] dismiss clicked ${card.session_id.slice(0, 8)} epoch=${card.epoch}`)
    await invoke('dismiss_notification', { sessionId: card.session_id, epoch: card.epoch })
  }
  catch (error) {
    void logWarn(`[notif-ui] dismiss failed: ${String(error)}`)
    console.warn('failed to dismiss notification', error)
  }
}

onMounted(async () => {
  await refresh()
  unlisten = await listen<NotificationPayload>('notification:changed', (event) => {
    cards.value = event.payload.cards
  })
})

onUnmounted(() => {
  if (unlisten)
    void unlisten()
})
</script>

<template>
  <div class="notif-root">
    <transition-group
      name="notif"
      tag="div"
      class="notif-list"
    >
      <section
        v-for="card in cards"
        :key="`${card.session_id}:${card.epoch}`"
        class="notif-card"
      >
        <header class="notif-head">
          <span class="notif-agent">{{ agentLabel(card.agent) }}</span>
          <span class="notif-tag">{{ kindTag(card) }}</span>
        </header>

        <div class="notif-tool">
          {{ card.tool_name ?? (card.kind === 'choice' ? 'Question' : 'Permission') }}
        </div>
        <pre
          v-if="card.summary"
          class="notif-cmd"
        >{{ card.summary }}</pre>

        <div class="notif-meta">
          <span v-if="card.working_directory">📁 {{ folderName(card.working_directory) }}</span>
          <span class="notif-sid">{{ shortSession(card.session_id) }}</span>
        </div>

        <div class="notif-hint">
          {{ kindHint(card) }}
        </div>

        <div class="notif-actions">
          <button
            class="notif-dismiss"
            @click="dismiss(card)"
          >
            Dismiss
          </button>
        </div>
      </section>
    </transition-group>
  </div>
</template>

<style scoped>
.notif-root {
  width: 100vw;
  height: 100vh;
  box-sizing: border-box;
  padding: 10px;
  overflow-y: auto;
  background: transparent;
}

.notif-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.notif-card {
  box-sizing: border-box;
  padding: 12px 14px 10px;
  border-radius: 14px;
  border: 1px solid rgba(77, 107, 107, 0.18);
  background: rgba(252, 250, 245, 0.96);
  box-shadow: 0 14px 38px rgba(40, 60, 60, 0.2);
  backdrop-filter: blur(10px);
  color: #1f2a2a;
  -webkit-user-select: none;
  user-select: none;
}

.notif-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.notif-agent {
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: #5a7a72;
}

.notif-tag {
  font-size: 11px;
  font-weight: 600;
  color: #8a5a2a;
  background: rgba(240, 190, 120, 0.28);
  border-radius: 999px;
  padding: 2px 9px;
}

.notif-tool {
  font-size: 15px;
  font-weight: 700;
  line-height: 1.2;
}

.notif-cmd {
  margin: 7px 0 0;
  padding: 7px 9px;
  max-height: 72px;
  overflow: auto;
  border-radius: 9px;
  background: rgba(31, 42, 42, 0.06);
  font-family: 'SF Mono', 'Menlo', monospace;
  font-size: 12px;
  line-height: 1.4;
  white-space: pre-wrap;
  word-break: break-all;
}

.notif-meta {
  display: flex;
  gap: 10px;
  align-items: center;
  margin-top: 7px;
  font-size: 11px;
  color: #6a7a78;
}

.notif-sid {
  font-family: 'SF Mono', 'Menlo', monospace;
}

.notif-hint {
  margin-top: 8px;
  font-size: 12px;
  color: #4d6b6b;
}

.notif-actions {
  display: flex;
  justify-content: flex-end;
  margin-top: 10px;
}

.notif-dismiss {
  padding: 7px 16px;
  border: none;
  border-radius: 10px;
  background: rgba(31, 42, 42, 0.08);
  color: #44514f;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: filter 0.12s ease, transform 0.06s ease;
}

.notif-dismiss:hover {
  filter: brightness(0.97);
}

.notif-dismiss:active {
  transform: translateY(1px);
}

.notif-enter-active,
.notif-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}

.notif-enter-from,
.notif-leave-to {
  opacity: 0;
  transform: translateY(-6px);
}
</style>
