import type { Ref } from 'vue'
import type { SpriteViewportRect } from '../live2d/runtime'
import { invoke } from '@tauri-apps/api/core'
import { onMounted, onUnmounted } from 'vue'

export interface InteractiveRegion {
  x: number
  y: number
  width: number
  height: number
}

/// Elements carrying this attribute (speech bubble, completion toasts,
/// context menu) are reported as mouse-interactive; everywhere else the
/// window becomes click-through. All of them are v-if gated, so DOM
/// presence equals visibility.
const REGION_SELECTOR = '[data-interactive-region]'
const SYNC_INTERVAL_MS = 200
/** Extra logical pixels around the model so it stays easy to grab. */
const SPRITE_GRAB_MARGIN = 4

export interface UseInteractiveRegionsOptions {
  canvasRef: Ref<HTMLCanvasElement | undefined>
  getSpriteRect: () => SpriteViewportRect | null
}

export function useInteractiveRegions(options: UseInteractiveRegionsOptions) {
  let timer: ReturnType<typeof setInterval> | null = null
  let lastSerialized = ''

  function collectRegions(): InteractiveRegion[] {
    const regions: InteractiveRegion[] = []

    const canvas = options.canvasRef.value
    if (canvas) {
      const canvasRect = canvas.getBoundingClientRect()
      const spriteRect = options.getSpriteRect()
      if (spriteRect) {
        regions.push({
          x: canvasRect.left + spriteRect.x - SPRITE_GRAB_MARGIN,
          y: canvasRect.top + spriteRect.y - SPRITE_GRAB_MARGIN,
          width: spriteRect.width + SPRITE_GRAB_MARGIN * 2,
          height: spriteRect.height + SPRITE_GRAB_MARGIN * 2,
        })
      } else {
        // Model not fitted yet (loading or failed): keep the whole canvas
        // interactive so the pet can still be dragged / right-clicked.
        regions.push({
          x: canvasRect.left,
          y: canvasRect.top,
          width: canvasRect.width,
          height: canvasRect.height,
        })
      }
    }

    for (const element of document.querySelectorAll(REGION_SELECTOR)) {
      const rect = element.getBoundingClientRect()
      if (rect.width > 0 && rect.height > 0) {
        regions.push({ x: rect.left, y: rect.top, width: rect.width, height: rect.height })
      }
    }

    return regions.map(region => ({
      x: Math.floor(region.x),
      y: Math.floor(region.y),
      width: Math.ceil(region.width),
      height: Math.ceil(region.height),
    }))
  }

  function syncInteractiveRegions() {
    const regions = collectRegions()
    const serialized = JSON.stringify(regions)
    if (serialized === lastSerialized) {
      return
    }

    lastSerialized = serialized
    invoke('set_interactive_regions', { regions }).catch((error) => {
      // Reset so the next tick retries instead of silently staying stale.
      lastSerialized = ''
      console.warn('[cursor] failed to sync interactive regions', error)
    })
  }

  onMounted(() => {
    syncInteractiveRegions()
    timer = setInterval(syncInteractiveRegions, SYNC_INTERVAL_MS)
  })

  onUnmounted(() => {
    if (timer) {
      clearInterval(timer)
      timer = null
    }
  })

  return { syncInteractiveRegions }
}
