#!/usr/bin/env node
// copiwaifu hook for Claude Code (claude-only build).
//
// Maps CC hook events to copiwaifu events, narrows "attention" down to real
// permission/choice requests, POSTs to the local navigator server, and keeps
// an append-only trace at ~/.copiwaifu/logs/hook.log so "did CC even fire the
// hook?" is always answerable. Never throws, always exits 0.

const fs = require('node:fs')
const http = require('node:http')
const os = require('node:os')
const path = require('node:path')

const args = process.argv.slice(2)
const agent = args[args.indexOf('--agent') + 1]
const rawEvent = args[args.indexOf('--event') + 1]

if (!agent || !rawEvent) process.exit(0)

const CLAUDE_MAP = {
  SessionStart: 'session_start',
  SessionEnd: 'session_end',
  UserPromptSubmit: 'thinking',
  PreToolUse: 'tool_use',
  PostToolUse: 'tool_result',
  // A failed tool call is a normal mid-turn beat for CC, not a session error.
  PostToolUseFailure: 'tool_result',
  Stop: 'complete',
  Notification: 'permission_request',
  PermissionRequest: 'permission_request',
}

// PreToolUse on these tools means CC is waiting for the user to pick —
// surfaced as an attention card of kind "choice".
const CHOICE_TOOLS = new Set(['askuserquestion', 'exitplanmode'])

const HOME = os.homedir()
const PORT_FILES = [
  path.join(HOME, '.copiwaifu', 'port'),
  path.join(os.tmpdir(), 'copiwaifu-port'),
]
const SESSION_DIR = path.join(HOME, '.copiwaifu', 'sessions')
const LOG_DIR = path.join(HOME, '.copiwaifu', 'logs')
const LOG_FILE = path.join(LOG_DIR, 'hook.log')
const LOG_MAX_BYTES = 1024 * 1024

const startedAt = Date.now()
let handled = false

const chunks = []
process.stdin.on('data', c => chunks.push(c))
process.stdin.on('end', () => handle(Buffer.concat(chunks).toString('utf8')))
setTimeout(() => handle(''), 300)

function handle(input) {
  if (handled) return
  handled = true

  try {
    run(parseJson(input))
  } catch (err) {
    trace(`${rawEvent}→error ${String(err && err.message || err).slice(0, 120)}`)
    process.exit(0)
  }
}

function run(ctx) {
  const sessionId = ctx.session_id || ctx.sessionId || `${agent}-${process.ppid}`
  const rawTool = pickText(ctx.tool_name, ctx.toolName)

  let mappedEvent = CLAUDE_MAP[rawEvent] || null
  let attentionKind

  if (rawEvent === 'Notification') {
    if (isIdleNotification(ctx.message)) {
      // "Claude is waiting for your input" — idle chatter, not an approval.
      trace(`${rawEvent}→skip sid=${sid8(sessionId)} msg=${clip(ctx.message, 60)}`)
      return process.exit(0)
    }
    // Permission phrasing — and unknown Notification texts default here too
    // (better a spurious card than a missed approval).
    attentionKind = 'permission'
  }

  if (rawEvent === 'PermissionRequest') attentionKind = 'permission'

  if (rawEvent === 'PreToolUse' && rawTool && CHOICE_TOOLS.has(rawTool.toLowerCase())) {
    mappedEvent = 'permission_request'
    attentionKind = 'choice'
  }

  if (!mappedEvent) {
    trace(`${rawEvent}→unmapped sid=${sid8(sessionId)}`)
    return process.exit(0)
  }

  const needsAttention = mappedEvent === 'permission_request'
  const summary = resolveSummary(ctx, mappedEvent, attentionKind, rawTool)
  const workingDirectory = ctx.cwd || ctx.workingDirectory || ctx.working_directory
  const sessionTitle = resolveSessionTitle(ctx, mappedEvent)
  const turnStart = mappedEvent === 'thinking' && rawEvent === 'UserPromptSubmit'
  const turnFingerprint = turnStart ? (sessionTitle || summary) : undefined

  const data = {
    tool_name: rawTool,
    summary,
    working_directory: workingDirectory,
    session_title: sessionTitle,
    needs_attention: needsAttention,
    attention_kind: attentionKind,
    turn_start: turnStart,
    turn_fingerprint: turnFingerprint,
  }
  const payload = { agent, session_id: sessionId, event: mappedEvent, data }

  writeSession(sessionId, mappedEvent, workingDirectory, sessionTitle, needsAttention, {
    type: mappedEvent,
    timestamp: Date.now(),
    toolName: rawTool,
    summary,
    turnStart,
    turnFingerprint,
  })

  const tag = `${rawEvent}→${mappedEvent} sid=${sid8(sessionId)} tool=${rawTool || '-'} attn=${needsAttention ? attentionKind : '-'}`

  const port = readPort()
  if (!port) {
    trace(`${tag} post=skip(no-port) ${Date.now() - startedAt}ms`)
    return process.exit(0)
  }

  postJson(
    port,
    '/event',
    payload,
    800,
    () => {
      trace(`${tag} post=ok ${Date.now() - startedAt}ms`)
      process.exit(0)
    },
    (why) => {
      trace(`${tag} post=fail(${why}) ${Date.now() - startedAt}ms`)
      process.exit(0)
    },
  )
}

// ── Notification classification ───────────────────────────────────────────────

function isIdleNotification(message) {
  const msg = String(message || '').toLowerCase()
  return /waiting for (your )?input|is idle|idle for/.test(msg)
}

// ── Summary / title extraction ────────────────────────────────────────────────

const TRUNCATE_LIMITS = { complete: 512, error: 512, thinking: 180, permission_request: 200 }
const TRUNCATE_DEFAULT = 120

function resolveSummary(ctx, mappedEvent, attentionKind, rawTool) {
  const limit = TRUNCATE_LIMITS[mappedEvent] || TRUNCATE_DEFAULT

  if (attentionKind === 'choice') {
    const input = ctx.tool_input || ctx.toolInput || {}
    if (rawTool && rawTool.toLowerCase() === 'askuserquestion') {
      const first = Array.isArray(input.questions) ? input.questions[0] : undefined
      const text = first && pickText(first.question, first.header)
      return text ? truncate(text, limit) : 'Claude has a question for you'
    }
    return 'Plan ready for review'
  }

  if (mappedEvent === 'thinking') {
    const prompt = pickText(ctx.prompt, ctx.message, ctx.userPrompt, ctx.user_prompt)
    if (prompt) return truncate(firstNonEmptyLine(prompt), limit)
  }

  if (mappedEvent === 'complete') {
    const text = pickText(
      ctx.summary,
      ctx.description,
      ctx.last_assistant_message,
      ctx['last-assistant-message'],
      ctx.message,
      ctx.result,
    )
    if (text) return truncate(text, limit)
  }

  if (mappedEvent === 'permission_request') {
    const text = pickText(ctx.message, ctx.prompt, ctx.reason)
    if (text) return truncate(text, limit)
  }

  const explicit = pickText(ctx.summary, ctx.description, ctx['last-assistant-message'])
  if (explicit) return truncate(explicit, limit)

  const input = ctx.tool_input || ctx.toolInput || ctx.input
  if (typeof input === 'string') return truncate(input, limit)
  if (input && typeof input === 'object') {
    const preferred = input.command || input.file_path || input.path || input.prompt || input.query
    if (typeof preferred === 'string') return truncate(preferred, limit)
    return truncate(JSON.stringify(input), limit)
  }
  return `等待 ${agent} 操作`
}

function resolveSessionTitle(ctx, mappedEvent) {
  const limit = 180
  if (mappedEvent === 'thinking') {
    const prompt = pickText(ctx.prompt, ctx.message, ctx.userPrompt, ctx.user_prompt)
    if (prompt) return truncate(firstNonEmptyLine(prompt), limit)
  }
  return ctx.sessionTitle ? truncate(ctx.sessionTitle, limit) : undefined
}

// ── Session file (offline record + recovery source) ──────────────────────────

function writeSession(sessionId, ev, workDir, title, attention, lastEvent) {
  try {
    fs.mkdirSync(SESSION_DIR, { recursive: true })
    const safeId = sessionId.replace(/[^a-zA-Z0-9_-]/g, '_')
    const file = path.join(SESSION_DIR, `${agent}_${safeId}.json`)
    const existing = parseJson(tryRead(file))
    const STATUS_MAP = {
      session_start: 'idle', session_end: 'idle',
      thinking: 'working', tool_use: 'working', tool_result: 'working', permission_request: 'working',
      error: 'error', complete: 'completed',
    }
    const now = Date.now()
    const eventHistory = appendEventHistory(
      ev === 'session_start' ? [] : existing.events,
      { ...lastEvent, timestamp: lastEvent.timestamp || now },
      ev,
    )
    const sessionTitle = title || existing.sessionTitle
    const lastMeaningfulSummary = bestMeaningfulSummary(eventHistory, sessionTitle)
      || (ev === 'session_start' ? undefined : existing.lastMeaningfulSummary)
    const session = {
      sessionId,
      agent,
      status: STATUS_MAP[ev] || 'working',
      startedAt: existing.startedAt || now,
      lastUpdated: now,
      workingDirectory: workDir || existing.workingDirectory,
      sessionTitle,
      needsAttention: attention,
      lastEvent: eventHistory[eventHistory.length - 1] || lastEvent,
      events: eventHistory,
      lastMeaningfulSummary,
      aiTalkContext: ev === 'session_start' ? undefined : existing.aiTalkContext,
    }
    if (ev === 'session_end') session.endedAt = now
    const tmp = `${file}.tmp`
    fs.writeFileSync(tmp, JSON.stringify(session, null, 2))
    fs.renameSync(tmp, file)
  } catch {}
}

function appendEventHistory(existingEvents, event, ev) {
  const summary = typeof event.summary === 'string' ? truncate(event.summary.trim()) : undefined
  const toolName = typeof event.toolName === 'string' ? event.toolName.trim() : undefined
  const next = Array.isArray(existingEvents) ? existingEvents.slice(-19) : []
  next.push({
    type: ev,
    eventType: ev,
    timestamp: event.timestamp || Date.now(),
    timestampMs: event.timestamp || Date.now(),
    toolName,
    summary,
    turnStart: Boolean(event.turnStart),
    turnFingerprint: event.turnFingerprint,
    informative: isMeaningfulSummary(summary, toolName, ev),
  })
  return next
}

function bestMeaningfulSummary(events, sessionTitle) {
  const candidates = events
    .filter(event => event.informative && event.summary)
    .map(event => ({ event, priority: summaryPriority(event) }))
    .sort((a, b) => b.priority - a.priority || (b.event.timestampMs || 0) - (a.event.timestampMs || 0))

  const best = candidates[0]
  if (best?.priority >= 4) {
    return best.event.summary
  }

  if (sessionTitle) {
    return sessionTitle
  }

  return best?.event.summary
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

function isMeaningfulSummary(summary, toolName, ev) {
  if (!summary || !summary.trim()) return false
  const normalized = normalizeSummary(summary)
  if (!normalized) return false
  if (normalized === normalizeSummary(toolName || '')) return false
  if (normalized === normalizeSummary(agent) || normalized === normalizeSummary(ev)) return false
  if (['idle', 'working', 'complete', 'completed', 'error', 'thinking', 'tooluse', 'toolresult'].includes(normalized)) return false

  const lower = summary.trim().toLowerCase()
  if (lower.startsWith('waiting ') || lower.startsWith('waiting for ')) return false
  if (summary.trim().startsWith('等') && summary.includes('操作')) return false
  if (lower.startsWith('running ') || lower.startsWith('finished ')) return false
  if (lower.endsWith(' session started') || lower.endsWith(' session closed') || lower.endsWith(' session archived') || lower.endsWith(' finished this turn')) return false
  return true
}

function normalizeSummary(value) {
  return String(value || '').trim().toLowerCase().replace(/[^\p{Letter}\p{Number}]/gu, '')
}

// ── Small utilities ───────────────────────────────────────────────────────────

function pickText(...values) {
  for (const value of values) {
    if (typeof value !== 'string') continue
    const text = value.trim()
    if (text) return text
  }
  return undefined
}

function firstNonEmptyLine(value) {
  return value.split(/\r?\n/).map(line => line.trim()).find(Boolean) || value.trim()
}

function truncate(v, limit) {
  const max = limit || TRUNCATE_DEFAULT
  return v.length > max ? `${v.slice(0, max)}...` : v
}

function clip(value, max) {
  return String(value || '').replace(/\s+/g, ' ').slice(0, max)
}

function sid8(id) {
  return String(id).slice(0, 8)
}

function parseJson(s) { try { return JSON.parse(s) } catch { return {} } }
function tryRead(f) { try { return fs.readFileSync(f, 'utf8') } catch { return '{}' } }

// ── Trace log (~/.copiwaifu/logs/hook.log, 1MB self-rotation) ────────────────

function trace(line) {
  try {
    fs.mkdirSync(LOG_DIR, { recursive: true })
    try {
      if (fs.statSync(LOG_FILE).size > LOG_MAX_BYTES) {
        fs.renameSync(LOG_FILE, `${LOG_FILE}.1`)
      }
    } catch {}
    fs.appendFileSync(LOG_FILE, `${new Date().toISOString()} ${line}\n`)
  } catch {}
}

// ── Transport ─────────────────────────────────────────────────────────────────

function readPort() {
  for (const f of PORT_FILES) {
    try {
      const p = Number(fs.readFileSync(f, 'utf8').trim())
      if (Number.isInteger(p) && p > 0) return p
    } catch {}
  }
  return null
}

function postJson(port, route, payload, timeout, onSuccess, onFailure) {
  const body = JSON.stringify(payload)
  const req = http.request({
    host: '127.0.0.1', port, path: route, method: 'POST', timeout,
    headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
  }, (res) => {
    res.resume()
    res.statusCode >= 200 && res.statusCode < 300 ? onSuccess() : onFailure(`http-${res.statusCode}`)
  })
  req.on('error', err => onFailure(err.code || 'error'))
  req.on('timeout', () => { req.destroy(); onFailure('timeout') })
  req.end(body)
}
