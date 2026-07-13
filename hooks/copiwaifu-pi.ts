// Copiwaifu extension for pi-coding-agent.
// Maps pi's extension lifecycle to Copiwaifu's canonical event stream.
// It is observational only: failures talking to Copiwaifu never affect pi.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const PORT_FILES = [
  path.join(os.homedir(), '.copiwaifu', 'port'),
  path.join(os.tmpdir(), 'copiwaifu-port'),
]
const SESSION_DIR = path.join(os.homedir(), '.copiwaifu', 'sessions')
const AGENT = 'pi'
const MAX_SUMMARY = 180
const MAX_HISTORY = 20

export default function (pi: any) {
  let sessionId = `pi-${process.pid}`
  let workingDirectory = process.cwd()
  let sessionTitle: string | undefined
  let latestAssistantText: string | undefined
  let runFailed = false
  let pending = Promise.resolve()

  function sessionContext(extra: Record<string, unknown> = {}) {
    return {
      working_directory: workingDirectory,
      session_title: sessionTitle,
      needs_attention: false,
      ...extra,
    }
  }

  function enqueue(event: string, data: Record<string, unknown> = {}) {
    writeSession(sessionId, event, data)
    const payload = { agent: AGENT, session_id: sessionId, event, data }
    pending = pending
      .then(async () => {
        const port = readPort()
        if (port) await postJson(port, payload)
      })
      .catch(() => {})
    return pending
  }

  pi.on('session_start', async (_event: any, ctx: any) => {
    sessionId = safe(() => ctx.sessionManager.getSessionId()) || `pi-${process.pid}`
    workingDirectory = ctx.cwd || safe(() => ctx.sessionManager.getCwd()) || process.cwd()
    sessionTitle = redactSensitive(safe(() => ctx.sessionManager.getSessionName()) || '') || undefined
    latestAssistantText = undefined
    runFailed = false

    await enqueue('session_start', sessionContext({
      summary: sessionTitle || 'Pi session started',
      tool_name: 'Pi',
    }))
  })

  pi.on('session_info_changed', async (event: any) => {
    sessionTitle = event.name ? redactSensitive(event.name) : undefined
  })

  pi.on('before_agent_start', async (event: any) => {
    const summary = truncate(firstNonEmptyLine(event.prompt), MAX_SUMMARY)
    if (summary && !sessionTitle) sessionTitle = summary
    latestAssistantText = undefined
    runFailed = false

    await enqueue('thinking', sessionContext({
      summary,
      tool_name: 'Pi',
      turn_start: true,
      turn_fingerprint: summary,
    }))
  })

  pi.on('agent_start', async () => {
    runFailed = false
  })

  pi.on('tool_execution_start', async (event: any) => {
    await enqueue('tool_use', sessionContext({
      tool_name: event.toolName,
      summary: summarizeToolInput(event.args) || `Running ${event.toolName}`,
    }))
  })

  pi.on('tool_execution_end', async (event: any) => {
    await enqueue('tool_result', sessionContext({
      tool_name: event.toolName,
      summary: summarizeToolResult(event.result) || `${event.isError ? 'Failed' : 'Finished'} ${event.toolName}`,
    }))
  })

  pi.on('message_end', async (event: any) => {
    const message = event.message
    if (message?.role !== 'assistant') return
    latestAssistantText = extractMessageText(message) || message.errorMessage
    if (message.stopReason === 'error' || message.errorMessage) runFailed = true
  })

  pi.on('agent_end', async (event: any) => {
    const assistant = [...(event.messages || [])].reverse().find((message: any) => message?.role === 'assistant')
    if (assistant && (assistant.stopReason === 'error' || assistant.errorMessage)) runFailed = true
  })

  pi.on('agent_settled', async () => {
    const summary = truncate(
      firstNonEmptyLine(latestAssistantText) || (runFailed ? 'Pi encountered an error' : 'Pi finished this turn'),
      512,
    )
    await enqueue(runFailed ? 'error' : 'complete', sessionContext({
      summary,
      tool_name: 'Pi',
    }))
    runFailed = false
  })

  pi.on('session_shutdown', async () => {
    await enqueue('session_end', sessionContext({
      summary: 'Pi session closed',
      tool_name: 'Pi',
    }))
    await pending
  })
}

function readPort() {
  for (const file of PORT_FILES) {
    try {
      const port = Number(fs.readFileSync(file, 'utf8').trim())
      if (Number.isInteger(port) && port > 0) return port
    } catch {}
  }
  return null
}

async function postJson(port: number, payload: unknown) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), 800)
  try {
    const response = await fetch(`http://127.0.0.1:${port}/event`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
      signal: controller.signal,
    })
    return response.ok
  } catch {
    return false
  } finally {
    clearTimeout(timer)
  }
}

function writeSession(currentSessionId: string, event: string, data: Record<string, unknown>) {
  try {
    fs.mkdirSync(SESSION_DIR, { recursive: true })
    const safeId = currentSessionId.replace(/[^a-zA-Z0-9_-]/g, '_')
    const file = path.join(SESSION_DIR, `${AGENT}_${safeId}.json`)
    const existing = tryReadJson(file)
    const now = Date.now()
    const events = appendEventHistory(
      event === 'session_start' ? [] : existing.events,
      event,
      data,
      now,
    )
    const summary = bestMeaningfulSummary(events, data.session_title || existing.sessionTitle)
    const session: Record<string, unknown> = {
      sessionId: currentSessionId,
      agent: AGENT,
      status: {
        session_start: 'idle',
        session_end: 'idle',
        thinking: 'working',
        tool_use: 'working',
        tool_result: 'working',
        error: 'error',
        complete: 'completed',
      }[event] || 'working',
      startedAt: existing.startedAt || now,
      lastUpdated: now,
      workingDirectory: data.working_directory || existing.workingDirectory,
      sessionTitle: data.session_title || existing.sessionTitle,
      needsAttention: data.needs_attention ?? existing.needsAttention ?? false,
      lastEvent: events[events.length - 1],
      events,
      lastMeaningfulSummary: summary || existing.lastMeaningfulSummary,
      aiTalkContext: existing.aiTalkContext,
    }
    if (event === 'session_end') session.endedAt = now

    const temp = `${file}.tmp`
    fs.writeFileSync(temp, JSON.stringify(session, null, 2))
    fs.renameSync(temp, file)
  } catch {}
}

function appendEventHistory(existing: unknown, event: string, data: Record<string, unknown>, timestamp: number) {
  const summary = truncate(String(data.summary || '').trim()) || undefined
  const toolName = truncate(String(data.tool_name || '').trim()) || undefined
  const next = Array.isArray(existing) ? existing.slice(-MAX_HISTORY + 1) : []
  next.push({
    type: event,
    eventType: event,
    timestamp,
    timestampMs: timestamp,
    toolName,
    summary,
    turnStart: Boolean(data.turn_start),
    turnFingerprint: data.turn_fingerprint,
    informative: isMeaningfulSummary(summary, toolName, event),
  })
  return next
}

function bestMeaningfulSummary(events: any[], title: unknown) {
  const candidates = events
    .filter(event => event.informative && event.summary)
    .map(event => ({ event, priority: summaryPriority(event) }))
    .sort((a, b) => b.priority - a.priority || b.event.timestampMs - a.event.timestampMs)
  const best = candidates[0]
  return best?.priority >= 4 ? best.event.summary : (typeof title === 'string' && title) || best?.event.summary
}

function summaryPriority(event: any) {
  if (event.type === 'complete' || event.type === 'error') return 5
  if (event.type === 'thinking') return 4
  if (event.type === 'tool_result') return 2
  if (event.type === 'tool_use') return 1
  return 0
}

function isMeaningfulSummary(summary: string | undefined, toolName: string | undefined, event: string) {
  if (!summary) return false
  const normalized = normalizeSummary(summary)
  if (!normalized || normalized === normalizeSummary(toolName || '') || normalized === normalizeSummary(event)) return false
  if (['idle', 'working', 'complete', 'completed', 'error', 'thinking', 'tooluse', 'toolresult', 'sessionstart', 'sessionend'].includes(normalized)) return false
  const lower = summary.toLowerCase()
  return !lower.startsWith('waiting ') && !lower.startsWith('running ') && !lower.startsWith('finished ')
}

function extractMessageText(message: any) {
  if (typeof message?.content === 'string') return message.content
  if (!Array.isArray(message?.content)) return undefined
  return message.content
    .filter((part: any) => part?.type === 'text')
    .map((part: any) => part.text)
    .filter(Boolean)
    .join(' ') || undefined
}

function summarizeToolInput(input: any) {
  if (!input || typeof input !== 'object') return undefined
  return summarizeValue(input, ['command', 'path', 'file_path', 'filePath', 'pattern', 'query', 'prompt', 'description'])
}

function summarizeToolResult(result: any) {
  if (!result || typeof result !== 'object') return undefined
  return summarizeValue(result, ['output', 'summary', 'message', 'error', 'stdout', 'stderr', 'text'])
}

function summarizeValue(value: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    if (typeof value[key] === 'string' && value[key].trim()) return truncate(firstNonEmptyLine(value[key] as string))
  }
  return undefined
}

function firstNonEmptyLine(value: unknown) {
  const text = String(value || '')
  return text.split(/\r?\n/).map(line => line.trim()).find(Boolean) || text.trim()
}

function truncate(value: string, limit = MAX_SUMMARY) {
  const safeValue = redactSensitive(value)
  return safeValue.length > limit ? `${safeValue.slice(0, limit)}...` : safeValue
}

function redactSensitive(value: string) {
  return value
    .replace(/\bBearer\s+[A-Za-z0-9._~+\/-]+/gi, 'Bearer [redacted]')
    .replace(/\b(?:sk|rk|ghp|github_pat|xox[baprs])-?[A-Za-z0-9_-]{8,}\b/gi, '[redacted]')
    .replace(/\bAKIA[0-9A-Z]{16}\b/g, '[redacted]')
    .replace(/(["']?\b(?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|private[_-]?key|authorization|token|password|secret)\b["']?\s*[:=])[^\r\n}]*/gi, '$1 [redacted]')
}

function normalizeSummary(value: string) {
  return value.trim().toLowerCase().replace(/[^\p{Letter}\p{Number}]/gu, '')
}

function tryReadJson(file: string) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, any>
  } catch {
    return {}
  }
}

function safe<T>(read: () => T) {
  try {
    return read()
  } catch {
    return undefined
  }
}
