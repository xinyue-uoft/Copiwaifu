// copiwaifu-opencode-plugin
// version: v2
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'

const PORT_FILES = [
  path.join(os.homedir(), '.copiwaifu', 'port'),
  path.join(os.tmpdir(), 'copiwaifu-port'),
]
const SESSION_DIR = path.join(os.homedir(), '.copiwaifu', 'sessions')

function readPort() {
  for (const file of PORT_FILES) {
    try {
      const port = Number(fs.readFileSync(file, 'utf8').trim())
      if (Number.isInteger(port) && port > 0) {
        return port
      }
    }
    catch {}
  }
  return null
}

function postJson(port, payload) {
  return new Promise((resolve) => {
    const body = JSON.stringify(payload)
    const req = http.request({
      host: '127.0.0.1',
      port,
      path: '/event',
      method: 'POST',
      timeout: 1000,
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(body),
      },
    }, (res) => {
      res.resume()
      resolve(res.statusCode >= 200 && res.statusCode < 300)
    })

    req.on('error', () => resolve(false))
    req.on('timeout', () => {
      req.destroy()
      resolve(false)
    })
    req.end(body)
  })
}

function truncate(value, max = 180) {
  if (!value) {
    return undefined
  }
  const text = String(value).trim()
  if (!text) {
    return undefined
  }
  return text.length > max ? `${text.slice(0, max)}...` : text
}

function tryReadJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'))
  }
  catch {
    return {}
  }
}

function writeSession(sessionId, event, data) {
  try {
    fs.mkdirSync(SESSION_DIR, { recursive: true })
    const safeId = sessionId.replace(/[^a-zA-Z0-9_-]/g, '_')
    const file = path.join(SESSION_DIR, `opencode_${safeId}.json`)
    const existing = tryReadJson(file)
    const now = Date.now()
    const statusMap = {
      session_start: 'idle',
      session_end: 'idle',
      thinking: 'working',
      tool_use: 'working',
      tool_result: 'working',
      permission_request: 'working',
      error: 'error',
      complete: 'completed',
    }
    const events = appendEventHistory(
      event === 'session_start' ? [] : existing.events,
      {
        type: event,
        timestamp: now,
        toolName: data.tool_name,
        summary: data.summary,
        turnStart: data.turn_start,
        turnFingerprint: data.turn_fingerprint,
      },
    )
    const sessionTitle = data.session_title || existing.sessionTitle
    const lastMeaningfulSummary = bestMeaningfulSummary(events, sessionTitle)
      || (event === 'session_start' ? undefined : existing.lastMeaningfulSummary)
    const session = {
      sessionId,
      agent: 'opencode',
      status: statusMap[event] || 'working',
      startedAt: existing.startedAt || now,
      lastUpdated: now,
      workingDirectory: data.working_directory || existing.workingDirectory,
      sessionTitle,
      needsAttention: data.needs_attention ?? existing.needsAttention ?? false,
      lastEvent: events[events.length - 1],
      events,
      lastMeaningfulSummary,
      aiTalkContext: event === 'session_start' ? undefined : existing.aiTalkContext,
    }
    if (event === 'session_end') {
      session.endedAt = now
    }
    const tmp = `${file}.tmp`
    fs.writeFileSync(tmp, JSON.stringify(session, null, 2))
    fs.renameSync(tmp, file)
  }
  catch {}
}

const EVENT_HISTORY_CAP = 100

function appendEventHistory(existingEvents, event) {
  const summary = truncate(event.summary, event.type === 'thinking' ? 600 : 180)
  const toolName = truncate(event.toolName)
  const next = Array.isArray(existingEvents) ? existingEvents.slice() : []
  next.push({
    type: event.type,
    eventType: event.type,
    timestamp: event.timestamp || Date.now(),
    timestampMs: event.timestamp || Date.now(),
    toolName,
    summary,
    turnStart: Boolean(event.turnStart),
    turnFingerprint: event.turnFingerprint,
    informative: isMeaningfulSummary(summary, toolName, event.type),
  })
  if (next.length > EVENT_HISTORY_CAP) {
    if (next[0]?.type === 'session_start') {
      const pinned = next[0]
      next.splice(0, next.length - (EVENT_HISTORY_CAP - 1))
      next.unshift(pinned)
    }
    else {
      next.splice(0, next.length - EVENT_HISTORY_CAP)
    }
  }
  return next
}

function bestMeaningfulSummary(events, sessionTitle) {
  const candidates = events
    .filter(item => item.informative && item.summary)
    .map(item => ({ item, priority: summaryPriority(item) }))
    .sort((a, b) => b.priority - a.priority || (b.item.timestampMs || 0) - (a.item.timestampMs || 0))
  const best = candidates[0]
  if (best?.priority >= 4) {
    return best.item.summary
  }
  if (sessionTitle) {
    return sessionTitle
  }
  return best?.item.summary
}

function summaryPriority(event) {
  if (event.type === 'complete' || event.eventType === 'complete') return 5
  if (event.type === 'error' || event.eventType === 'error') return 5
  if (event.type === 'thinking' || event.eventType === 'thinking') return 4
  if (event.type === 'permission_request' || event.eventType === 'permission_request') return 3
  if (event.type === 'tool_result' || event.eventType === 'tool_result') return 2
  if (event.type === 'tool_use' || event.eventType === 'tool_use') return 1
  return 0
}

function isMeaningfulSummary(summary, toolName, event) {
  if (!summary || !summary.trim()) {
    return false
  }

  const normalized = normalizeSummary(summary)
  if (!normalized) {
    return false
  }
  if (normalized === normalizeSummary(toolName || '')) {
    return false
  }
  if (normalized === 'opencode' || normalized === normalizeSummary(event)) {
    return false
  }
  if (['idle', 'working', 'complete', 'completed', 'error', 'thinking', 'tooluse', 'toolresult'].includes(normalized)) {
    return false
  }

  const lower = summary.trim().toLowerCase()
  if (lower.startsWith('waiting ') || lower.startsWith('waiting for ')) {
    return false
  }
  if (summary.trim().startsWith('等') && summary.includes('操作')) {
    return false
  }
  if (lower.startsWith('running ') || lower.startsWith('finished ')) {
    return false
  }
  if (lower.endsWith(' session started') || lower.endsWith(' session closed') || lower.endsWith(' session archived') || lower.endsWith(' finished this turn')) {
    return false
  }

  return true
}

function normalizeSummary(value) {
  return String(value || '').trim().toLowerCase().replace(/[^\p{Letter}\p{Number}]/gu, '')
}

function buildPayload(agent, sessionId, event, data) {
  return {
    agent,
    session_id: sessionId,
    event,
    data,
  }
}

function mapToolName(tool) {
  if (!tool) {
    return 'OpenCode'
  }
  return `${tool}`.charAt(0).toUpperCase() + `${tool}`.slice(1)
}

function firstDefined(...values) {
  return values.find(value => value !== undefined && value !== null && value !== '')
}

function formatPatterns(patterns) {
  if (Array.isArray(patterns)) {
    return patterns.filter(Boolean).join(' && ')
  }
  return patterns
}

function summarizeToolInput(input) {
  if (!input || typeof input !== 'object') {
    return undefined
  }
  return truncate(firstDefined(
    input.command,
    input.file_path,
    input.filePath,
    input.path,
    input.url,
    input.pattern,
    input.prompt,
    input.description,
  ))
}

function compactToolOutput(output) {
  return truncate(firstDefined(output?.title, output?.output, output?.error))
}

export default {
  id: 'copiwaifu',
  server: async ({ serverUrl }) => {
    const sessionCwd = new Map()
    const sessionTitle = new Map()
    const messageRoles = new Map()
    const latestAssistantText = new Map()
    const pendingPermissions = new Map()
    const PENDING_TTL_MS = 5 * 60 * 1000

    function markPending(sessionID) {
      if (sessionID) {
        pendingPermissions.set(sessionID, Date.now())
      }
    }

    function hasPending(sessionID) {
      const ts = pendingPermissions.get(sessionID)
      if (!ts) {
        return false
      }
      if (Date.now() - ts > PENDING_TTL_MS) {
        pendingPermissions.delete(sessionID)
        return false
      }
      return true
    }
    const recentEvents = new Map()
    const toolInputs = new Map()
    const toolNames = new Map()
    const reasoningStream = new Map()
    const localServerPort = serverUrl ? Number(serverUrl.port || 0) || null : null

    async function emit(event, sessionId, data = {}) {
      const payload = buildPayload('opencode', sessionId, event, data)
      writeSession(sessionId, event, data)

      const port = localServerPort || readPort()
      if (!port) {
        return
      }
      await postJson(port, payload)
    }

    function getSessionId(raw) {
      return raw ? `opencode-${raw}` : null
    }

    function rememberSession(rawSessionID, info = {}) {
      if (!rawSessionID) {
        return
      }
      if (info.directory) {
        sessionCwd.set(rawSessionID, info.directory)
      }
      if (info.title && !info.title.startsWith('New session')) {
        sessionTitle.set(rawSessionID, info.title)
      }
    }

    function allowEvent(key, windowMs = 1000) {
      const now = Date.now()
      for (const [cachedKey, timestamp] of recentEvents) {
        if (now - timestamp > 5000) {
          recentEvents.delete(cachedKey)
        }
      }
      const previous = recentEvents.get(key)
      if (previous && now - previous < windowMs) {
        return false
      }
      recentEvents.set(key, now)
      return true
    }

    async function emitOnce(key, event, sessionId, data = {}, windowMs) {
      if (!sessionId || !allowEvent(key, windowMs)) {
        return
      }
      await emit(event, sessionId, data)
    }

    async function emitThinking(rawSessionID, summary, extra = {}) {
      const sessionId = getSessionId(rawSessionID)
      await emitOnce(
        `thinking:${rawSessionID}:${normalizeSummary(summary || 'opencode is thinking')}`,
        'thinking',
        sessionId,
        {
          working_directory: sessionCwd.get(rawSessionID),
          session_title: truncate(sessionTitle.get(rawSessionID)),
          summary: truncate(summary, 600) || 'OpenCode is thinking',
          tool_name: 'OpenCode',
          needs_attention: false,
          ...extra,
        },
        250,
      )
    }

    async function emitToolUse(rawSessionID, callID, tool, input) {
      const sessionId = getSessionId(rawSessionID)
      const toolName = mapToolName(tool)
      await emitOnce(
        `tool:${rawSessionID}:${callID || toolName}:use`,
        'tool_use',
        sessionId,
        {
          working_directory: sessionCwd.get(rawSessionID),
          session_title: truncate(sessionTitle.get(rawSessionID)),
          summary: summarizeToolInput(input) || `Running ${toolName}`,
          tool_name: toolName,
          needs_attention: false,
        },
      )
    }

    async function emitToolResult(rawSessionID, callID, tool, output, failed = false) {
      const sessionId = getSessionId(rawSessionID)
      const toolName = mapToolName(tool)
      await emitOnce(
        `tool:${rawSessionID}:${callID || toolName}:${failed ? 'error' : 'result'}`,
        failed ? 'error' : 'tool_result',
        sessionId,
        {
          working_directory: sessionCwd.get(rawSessionID),
          session_title: truncate(sessionTitle.get(rawSessionID)),
          summary: compactToolOutput(output) || `${failed ? 'Failed' : 'Finished'} ${toolName}`,
          tool_name: toolName,
          needs_attention: false,
        },
      )
    }

    async function emitPermissionRequest(input) {
      const rawSessionID = input?.sessionID
      if (!rawSessionID) {
        return
      }
      const sessionId = getSessionId(rawSessionID)
      const toolName = mapToolName(input?.permission || input?.type)
      const summary = truncate(
        formatPatterns(firstDefined(input?.patterns, input?.pattern))
        || input?.title
        || input?.metadata?.description,
      ) || `OpenCode requests ${toolName}`
      markPending(rawSessionID)
      await emitOnce(
        `permission:${rawSessionID}:${input?.id || input?.requestID || summary}`,
        'permission_request',
        sessionId,
        {
          working_directory: sessionCwd.get(rawSessionID),
          session_title: truncate(sessionTitle.get(rawSessionID)),
          summary,
          tool_name: toolName,
          needs_attention: true,
        },
      )
    }

    function clearPendingPermission(rawSessionID) {
      if (rawSessionID) {
        pendingPermissions.delete(rawSessionID)
      }
    }

    return {
      'permission.ask': async (input, output) => {
        if (output?.status !== 'ask') {
          return
        }
        await emitPermissionRequest(input)
      },

      'tool.execute.before': async (input, output) => {
        const toolKey = `${input.sessionID}:${input.callID}`
        toolInputs.set(toolKey, output.args)
        toolNames.set(toolKey, input.tool)
        if (toolInputs.size > 300) {
          const oldest = toolInputs.keys().next().value
          toolInputs.delete(oldest)
          toolNames.delete(oldest)
        }
        await emitToolUse(input.sessionID, input.callID, input.tool, output.args)
      },

      'tool.execute.after': async (input, output) => {
        clearPendingPermission(input.sessionID)
        await emitToolResult(input.sessionID, input.callID, input.tool, output)
        toolInputs.delete(`${input.sessionID}:${input.callID}`)
        toolNames.delete(`${input.sessionID}:${input.callID}`)
      },

      event: async ({ event }) => {
        const type = event?.type
        const properties = event?.properties || {}

        if (type === 'session.created' && (properties.sessionID || properties.info?.id)) {
          const rawSessionID = properties.sessionID || properties.info.id
          const sessionId = getSessionId(rawSessionID)
          const cwd = properties.info.directory || undefined
          rememberSession(rawSessionID, properties.info)
          await emit('session_start', sessionId, {
            working_directory: cwd,
            session_title: truncate(properties.info.title),
            summary: truncate(properties.info.title) || 'OpenCode session started',
            tool_name: 'OpenCode',
            needs_attention: false,
          })
          return
        }

        if (type === 'session.updated' && (properties.sessionID || properties.info?.id)) {
          const rawSessionID = properties.sessionID || properties.info.id
          const coldStart = !sessionCwd.has(rawSessionID)
          if (coldStart) {
            const sessionId = getSessionId(rawSessionID)
            await emit('session_start', sessionId, {
              working_directory: properties.info.directory || undefined,
              session_title: truncate(properties.info.title),
              summary: truncate(properties.info.title) || 'OpenCode session resumed',
              tool_name: 'OpenCode',
              needs_attention: false,
            })
          }
          rememberSession(rawSessionID, properties.info)
          if (properties.info.time?.archived) {
            const sessionId = getSessionId(rawSessionID)
            await emit('session_end', sessionId, {
              working_directory: sessionCwd.get(rawSessionID),
              session_title: truncate(sessionTitle.get(rawSessionID) || properties.info.title),
              summary: 'OpenCode session archived',
              tool_name: 'OpenCode',
              needs_attention: false,
            })
          }
          return
        }

        if (type === 'session.deleted' && (properties.sessionID || properties.info?.id)) {
          const rawSessionID = properties.sessionID || properties.info.id
          const sessionId = getSessionId(rawSessionID)
          await emit('session_end', sessionId, {
            working_directory: sessionCwd.get(rawSessionID),
            session_title: truncate(sessionTitle.get(rawSessionID)),
            summary: 'OpenCode session closed',
            tool_name: 'OpenCode',
            needs_attention: false,
          })
          return
        }

        if (type === 'session.status' && properties.sessionID && properties.status?.type === 'idle') {
          if (hasPending(properties.sessionID)) {
            return
          }
          const sessionId = getSessionId(properties.sessionID)
          await emitOnce(
            `complete:${properties.sessionID}`,
            'complete',
            sessionId,
            {
              working_directory: sessionCwd.get(properties.sessionID),
              session_title: truncate(sessionTitle.get(properties.sessionID)),
              summary: truncate(latestAssistantText.get(properties.sessionID)) || 'OpenCode finished this turn',
              tool_name: 'OpenCode',
              needs_attention: false,
            },
            2000,
          )
          return
        }

        if (type === 'session.idle' && properties.sessionID) {
          pendingPermissions.delete(properties.sessionID)
          const sessionId = getSessionId(properties.sessionID)
          await emitOnce(
            `complete:${properties.sessionID}`,
            'complete',
            sessionId,
            {
              working_directory: sessionCwd.get(properties.sessionID),
              session_title: truncate(sessionTitle.get(properties.sessionID)),
              summary: truncate(latestAssistantText.get(properties.sessionID)) || 'OpenCode finished this turn',
              tool_name: 'OpenCode',
              needs_attention: false,
            },
            2000,
          )
          return
        }

        if (type === 'message.part.delta' && properties.sessionID) {
          if (properties.field === 'reasoning_content' && properties.delta) {
            const key = `${properties.sessionID}:${properties.partID || ''}`
            const acc = (reasoningStream.get(key) || '') + properties.delta
            reasoningStream.set(key, acc)
            if (reasoningStream.size > 100) {
              reasoningStream.delete(reasoningStream.keys().next().value)
            }
            const sessionId = getSessionId(properties.sessionID)
            await emitOnce(
              `thinking-stream:${properties.sessionID}`,
              'thinking',
              sessionId,
              {
                working_directory: sessionCwd.get(properties.sessionID),
                session_title: truncate(sessionTitle.get(properties.sessionID)),
                summary: truncate(acc, 600) || 'OpenCode is thinking',
                tool_name: 'OpenCode',
                needs_attention: false,
              },
              800,
            )
          }
          return
        }

        if (type === 'message.updated' && properties.info?.id && (properties.info?.sessionID || properties.sessionID)) {
          const rawSessionID = properties.info.sessionID || properties.sessionID
          messageRoles.set(properties.info.id, {
            role: properties.info.role,
            sessionID: rawSessionID,
          })
          if (messageRoles.size > 300) {
            messageRoles.delete(messageRoles.keys().next().value)
          }
          return
        }

        if (type === 'message.part.updated' && properties.part?.sessionID && properties.part?.type === 'reasoning') {
          await emitThinking(properties.part.sessionID, properties.part.text || properties.delta || 'OpenCode is thinking')
          return
        }

        if (type === 'message.part.updated' && properties.part?.messageID && properties.part?.type === 'text') {
          const meta = messageRoles.get(properties.part.messageID)
          if (!meta) {
            return
          }
          const sessionId = getSessionId(meta.sessionID)
          const text = truncate(properties.part.text)
          if (meta.role === 'user' && text) {
            await emit('thinking', sessionId, {
              working_directory: sessionCwd.get(meta.sessionID),
              session_title: truncate(sessionTitle.get(meta.sessionID)),
              summary: text,
              tool_name: 'OpenCode',
              needs_attention: false,
              turn_start: true,
              turn_fingerprint: text,
            })
            return
          }
          if (meta.role === 'assistant' && text) {
            latestAssistantText.set(meta.sessionID, text)
          }
          return
        }

        if (type === 'message.part.updated' && properties.part?.sessionID && properties.part?.type === 'tool') {
          const status = properties.part.state?.status
          if (status === 'running' || status === 'pending') {
            pendingPermissions.delete(properties.part.sessionID)
            await emitToolUse(properties.part.sessionID, properties.part.callID, properties.part.tool, properties.part.state?.input)
            return
          }
          if (status === 'completed') {
            await emitToolResult(properties.part.sessionID, properties.part.callID, properties.part.tool, properties.part.state)
            return
          }
          if (status === 'error') {
            await emitToolResult(properties.part.sessionID, properties.part.callID, properties.part.tool, properties.part.state, true)
          }
          return
        }

        if (type === 'permission.asked' && properties.sessionID) {
          await emitPermissionRequest(properties)
          return
        }

        if (type === 'permission.updated' && properties.sessionID) {
          await emitPermissionRequest(properties)
          return
        }

        if (type === 'permission.replied' && properties.sessionID) {
          clearPendingPermission(properties.sessionID)
          return
        }

        if (type === 'question.asked' && properties.sessionID) {
          const sessionId = getSessionId(properties.sessionID)
          const firstQuestion = properties.questions?.find?.(question => question?.question)?.question
          markPending(properties.sessionID)
          await emit('permission_request', sessionId, {
            working_directory: sessionCwd.get(properties.sessionID),
            session_title: truncate(sessionTitle.get(properties.sessionID)),
            summary: truncate(firstQuestion) || 'OpenCode needs your input',
            tool_name: 'AskUserQuestion',
            needs_attention: true,
          })
          return
        }

        if ((type === 'question.replied' || type === 'question.rejected') && properties.sessionID) {
          clearPendingPermission(properties.sessionID)
        }
      },
    }
  },
}
