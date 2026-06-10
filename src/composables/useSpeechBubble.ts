import { ref } from 'vue'
import { APP_LANGUAGE, WINDOW_SIZE_PRESET } from '../types/agent'
import type { AppLanguage, WindowSizePreset } from '../types/agent'

const MAX_TEXT_LENGTH = 100
const TYPING_SPEED = 60
const DEFAULT_DURATION = 3000

/**
 * Bubble priority classes. A new message is accepted only if its class is >=
 * the active message's class (or the active hold expired / same key). This is
 * what keeps "thinking started" and "work complete" bubbles on screen for
 * ~60s without tool-use chatter knocking them off.
 */
export const BUBBLE_CLASS = {
  TRANSIENT: 1,
  PINNED: 2,
  URGENT: 3,
} as const

export type BubbleClass = typeof BUBBLE_CLASS[keyof typeof BUBBLE_CLASS]

export interface SayOptions {
  /** How long the bubble stays after typing finishes (ms). */
  duration?: number
  cls?: BubbleClass
  /**
   * Identity of the message. A say() with the same key always replaces the
   * active bubble (AI-talk text swapping in over its own fallback), and
   * release(key) drops the hold early (attention resolved in CC).
   */
  key?: string
}

const AI_TALK_LIMITS: Record<WindowSizePreset, { cjk: number, latin: number }> = {
  [WINDOW_SIZE_PRESET.TINY]: { cjk: 24, latin: 45 },
  [WINDOW_SIZE_PRESET.SMALL]: { cjk: 36, latin: 70 },
  [WINDOW_SIZE_PRESET.MEDIUM]: { cjk: 42, latin: 80 },
  [WINDOW_SIZE_PRESET.LARGE]: { cjk: 60, latin: 110 },
  [WINDOW_SIZE_PRESET.HUGE]: { cjk: 80, latin: 140 },
}

export function useSpeechBubble() {
  const isVisible = ref(false)
  const displayedText = ref('')

  let fullText = ''
  let charIndex = 0
  let typingTimer: ReturnType<typeof setInterval> | null = null
  let hideTimer: ReturnType<typeof setTimeout> | null = null

  let activeCls: BubbleClass = BUBBLE_CLASS.TRANSIENT
  let activeKey = ''
  let activeUntil = 0
  let currentDuration = DEFAULT_DURATION

  function clearTimers() {
    if (typingTimer) {
      clearInterval(typingTimer)
      typingTimer = null
    }
    if (hideTimer) {
      clearTimeout(hideTimer)
      hideTimer = null
    }
  }

  function hide() {
    clearTimers()
    isVisible.value = false
    displayedText.value = ''
    activeCls = BUBBLE_CLASS.TRANSIENT
    activeKey = ''
    activeUntil = 0
  }

  /** Returns true if the bubble accepted (and now shows) the message. */
  function say(text: string, options: SayOptions = {}): boolean {
    const duration = options.duration ?? DEFAULT_DURATION
    const cls = options.cls ?? BUBBLE_CLASS.TRANSIENT
    const key = options.key ?? ''
    const now = Date.now()

    const activeHolds = isVisible.value && now < activeUntil
    const sameKey = key !== '' && key === activeKey
    if (activeHolds && !sameKey && cls < activeCls) {
      return false
    }

    clearTimers()
    activeCls = cls
    activeKey = key
    activeUntil = now + duration
    currentDuration = duration

    fullText = text.length > MAX_TEXT_LENGTH
      ? `${text.slice(0, MAX_TEXT_LENGTH)}…`
      : text
    charIndex = 0
    displayedText.value = ''
    isVisible.value = true

    typingTimer = setInterval(() => {
      if (charIndex < fullText.length) {
        charIndex++
        displayedText.value = fullText.slice(0, charIndex)
      }
      else {
        clearInterval(typingTimer!)
        typingTimer = null
        hideTimer = setTimeout(hide, currentDuration)
      }
    }, TYPING_SPEED)

    return true
  }

  /**
   * Drop the active message's hold (e.g. the attention it announced was
   * resolved in CC). The bubble fades out shortly instead of squatting for
   * the rest of its duration, and lower-class messages can show again.
   */
  function release(key?: string) {
    if (key !== undefined && key !== activeKey) {
      return
    }
    activeCls = BUBBLE_CLASS.TRANSIENT
    activeUntil = 0
    currentDuration = 600
    if (hideTimer) {
      clearTimeout(hideTimer)
      hideTimer = setTimeout(hide, 600)
    }
  }

  return { isVisible, displayedText, say, hide, release }
}

export function limitAiTalkBubbleText(
  text: string,
  windowSize: WindowSizePreset,
  language: AppLanguage,
) {
  const normalized = text
    .split(/\s+/)
    .filter(Boolean)
    .join(' ')
    .trim()
  if (!normalized) {
    return ''
  }

  const useCjkLimit = language === APP_LANGUAGE.CHINESE
    || language === APP_LANGUAGE.JAPANESE
    || /[\u3040-\u30ff\u3400-\u9fff]/.test(normalized)
  const limit = useCjkLimit
    ? AI_TALK_LIMITS[windowSize].cjk
    : AI_TALK_LIMITS[windowSize].latin

  const chars = [...normalized]
  if (chars.length <= limit) {
    return normalized
  }

  return `${chars.slice(0, Math.max(0, limit - 1)).join('')}…`
}
