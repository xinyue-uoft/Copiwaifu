import type { Live2DSprite } from 'easy-live2d'
import { Application } from 'pixi.js'
import { createLive2DModelSprite } from './model'

const SPRITE_FIT_PADDING = 8

export interface MountLive2DModelOptions {
  modelEntryUrl: string
  onReady?: (sprite: Live2DSprite) => void
}

export interface CreateLive2DRuntimeOptions {
  canvas: HTMLCanvasElement
  resolution?: number
  resizeTo?: Window | HTMLElement
}

function waitForNextFrame() {
  return new Promise<void>((resolve) => {
    requestAnimationFrame(() => {
      resolve()
    })
  })
}

export interface SpriteViewportRect {
  x: number
  y: number
  width: number
  height: number
}

function fitSpriteToViewport(sprite: Live2DSprite, width: number, height: number): SpriteViewportRect {
  const modelSize = sprite.getModelCanvasSize()
  const availableWidth = Math.max(1, width - SPRITE_FIT_PADDING * 2)
  const availableHeight = Math.max(1, height - SPRITE_FIT_PADDING * 2)

  if (!modelSize || modelSize.width <= 0 || modelSize.height <= 0) {
    sprite.width = availableWidth
    sprite.height = availableHeight
    sprite.x = SPRITE_FIT_PADDING
    sprite.y = SPRITE_FIT_PADDING
    return { x: sprite.x, y: sprite.y, width: availableWidth, height: availableHeight }
  }

  const scale = Math.min(availableWidth / modelSize.width, availableHeight / modelSize.height)
  const fittedWidth = modelSize.width * scale
  const fittedHeight = modelSize.height * scale

  sprite.width = fittedWidth
  sprite.height = fittedHeight
  sprite.x = Math.round((width - fittedWidth) / 2)
  sprite.y = Math.round(height - fittedHeight - SPRITE_FIT_PADDING)
  return { x: sprite.x, y: sprite.y, width: fittedWidth, height: fittedHeight }
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

/// Shape of the easy-live2d internals used by setGaze. The library exposes no
/// public gaze API, so we reach into the pointer handler's transform; every
/// access is guarded so a library update degrades to "no hover gaze".
interface GazeInternals {
  _pointerHandler?: {
    getCanvasPoint?: (ev: { clientX: number, clientY: number }) => { x: number, y: number } | null
    _viewTransform?: {
      transformViewX?: (x: number) => number
      transformViewY?: (y: number) => number
    }
  }
  _model?: {
    setDragging?: (x: number, y: number) => void
  }
}

export function createLive2DRuntime(options: CreateLive2DRuntimeOptions) {
  const app = new Application()
  let initialized = false
  let disposed = false
  let sprite: Live2DSprite | null = null
  let spriteRect: SpriteViewportRect | null = null
  let mountToken = 0
  let resizeToken = 0

  async function init() {
    if (initialized || disposed) {
      return
    }

    await app.init({
      canvas: options.canvas,
      backgroundAlpha: 0,
      autoDensity: true,
      resizeTo: options.resizeTo ?? window,
      resolution: options.resolution ?? Math.max(window.devicePixelRatio || 1, 1),
    })

    initialized = true
  }

  function getSprite() {
    return sprite
  }

  function detachSprite() {
    if (!sprite) {
      return
    }

    app.stage.removeChild(sprite as any)
    sprite.destroy()
    sprite = null
    spriteRect = null
  }

  function destroyModel() {
    mountToken += 1
    detachSprite()
  }

  async function syncSize() {
    if (!initialized || disposed || !sprite) {
      return
    }

    const token = ++resizeToken
    await waitForNextFrame()
    await waitForNextFrame()

    if (token !== resizeToken || disposed || !sprite) {
      return
    }

    const width = Math.round(options.canvas.clientWidth)
    const height = Math.round(options.canvas.clientHeight)
    if (width <= 0 || height <= 0) {
      return
    }

    spriteRect = fitSpriteToViewport(sprite, width, height)
    sprite.onResize()
  }

  /** Fitted model rect in canvas-local CSS pixels, null until the first fit. */
  function getSpriteRect() {
    return spriteRect
  }

  /**
   * Point the model's gaze at a window-local (client) coordinate. Driven by
   * the Rust global-cursor stream, so it works even while the window is
   * click-through and receives no native mouse events.
   */
  function setGaze(clientX: number, clientY: number) {
    if (!initialized || disposed || !sprite) {
      return
    }

    const internals = sprite as unknown as GazeInternals
    const handler = internals._pointerHandler
    const transform = handler?._viewTransform
    const model = internals._model
    if (!handler?.getCanvasPoint || !transform?.transformViewX || !transform.transformViewY || !model?.setDragging) {
      return
    }

    const point = handler.getCanvasPoint({ clientX, clientY })
    if (!point) {
      return
    }

    const viewX = clamp(transform.transformViewX(point.x), -1, 1)
    const viewY = clamp(transform.transformViewY(point.y), -1, 1)
    model.setDragging(viewX, viewY)
  }

  async function mountModel(mountOptions: MountLive2DModelOptions) {
    if (!initialized || disposed) {
      return null
    }

    const token = ++mountToken
    const nextSprite = await createLive2DModelSprite({
      modelEntryUrl: mountOptions.modelEntryUrl,
    })

    if (disposed || token !== mountToken) {
      nextSprite.destroy()
      return null
    }

    nextSprite.onLive2D('ready', () => {
      if (disposed || token !== mountToken || sprite !== nextSprite) {
        return
      }

      void syncSize()
      mountOptions.onReady?.(nextSprite)
    })

    detachSprite()

    if (disposed || token !== mountToken) {
      nextSprite.destroy()
      return null
    }

    app.stage.addChild(nextSprite as any)
    sprite = nextSprite
    await syncSize()

    return nextSprite
  }

  function dispose() {
    if (disposed) {
      return
    }

    disposed = true
    destroyModel()

    if (initialized) {
      app.destroy(true)
    }
  }

  return {
    init,
    getSprite,
    getSpriteRect,
    setGaze,
    syncSize,
    mountModel,
    destroyModel,
    dispose,
  }
}
