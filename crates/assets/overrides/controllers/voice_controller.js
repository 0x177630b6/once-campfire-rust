import { Controller } from "@hotwired/stimulus"

// Live voice ticket (a request, fault, complaint or incident, in any language) over Gemini Live,
// spoken directly from the browser.
//
// The server mints a single-use ephemeral token that locks the whole session setup (model,
// instructions, tools, transcription, resumption, compression), so the browser only ever sends
// `{"setup":{}}` (plus a resumption handle when reconnecting) and never sees the API key. The
// microphone is captured by voice/pcm-worklet.js (16 kHz Int16LE mono, ~100 ms chunks); the
// model answers with 24 kHz Int16LE mono audio and incremental transcriptions of both sides.
// When the model calls `submit_incident`, its arguments plus the accumulated transcript are
// POSTed to the report URL, which publishes the message in the room as the current user. When the
// page has an ask URL (HERMES_ASK_URL on the server), the model can also call `ask_hermes`: the
// question is POSTed there, the server asks the Hermes agent (shown as Sky), and the answer goes
// back to the model as the tool response, which it then speaks. The page shows each question as a
// small "Sky" line; those lines stay out of the report's transcript (the assistant's spoken answer is in it).
//
// The protocol lives in LiveSession (no DOM, no audio) so it can be exercised on its own; the
// Stimulus controller wires it to the microphone, the speaker and the page.
//
// Never store anything in `this.context`, `this.element`, `this.application`, `this.scope`,
// `this.data` or `this.targets` in the controller: they're Stimulus' own (an AudioContext once
// stored in `this.context` broke every target lookup).

const INPUT_RATE = 16000
const OUTPUT_RATE = 24000
const INPUT_MIME = `audio/pcm;rate=${INPUT_RATE}`
const FINISH_TIMEOUT_MS = 10000
// The server gives the bridge 60 s (and the bridge gives Hermes 55 s); a little more here.
const ASK_TIMEOUT_MS = 65000
const PLAYBACK_LEAD_S = 0.05

// The report's transcript says "Employee" (fixed English labels, like the report's, whatever language
// is spoken); the page says "You".
const LABELS = { user: "Employee", model: "Assistant" }
const SCREEN_LABELS = { user: "You", model: "Assistant" }

// Sent as text right after setup so the assistant speaks first (greets, asks what they need). Text
// input isn't transcribed, so it shows neither on the page nor in the report. In English, like the
// system instruction, which says which language to greet in.
export const KICKOFF_TEXT = "[The session starts: greet the employee briefly, in the language your instructions say, and ask what they need or what happened.]"

// Every start-up step has a deadline, so a stalled step ends with a message naming it instead of
// an endless "Connecting…".
const STEP_TIMEOUT_MS = { mic: 60000, audio: 10000, token: 15000, connect: 20000 }

export class StepTimeout extends Error {
  constructor(step) {
    super(`timeout: ${step}`)
    this.step = step
  }
}

export function withTimeout(promise, ms, step) {
  let timer
  const deadline = new Promise((_, reject) => { timer = setTimeout(() => reject(new StepTimeout(step)), ms) })
  return Promise.race([promise, deadline]).finally(() => clearTimeout(timer))
}

// For the "Technical details" of an error, not the sentence.
const STEP_LABELS = {
  mic: "microphone permission",
  audio: "starting the browser's audio",
  token: "opening a session on the MeshDuty server",
  connect: "connecting to Google's voice service (generativelanguage.googleapis.com)"
}

export const MESSAGES = {
  ready: "Tap the mic to start",
  insecure: "The microphone only works over HTTPS: open this page at an https:// address.",
  unsupported: "This browser can't hold a live voice conversation. Try a recent browser.",
  micDenied: "Microphone access is blocked.",
  micDeniedHelp: "Chrome: padlock in the address bar → Microphone → Allow.\niPhone: Settings → Safari → Microphone. Then try again.",
  micMissing: "No microphone found on this device.",
  micError: "Couldn't open the microphone.",
  micLost: "The microphone was cut off (an incoming call, another app or the screen locked).",
  stepMic: "Microphone permission…",
  stepConnect: "Connecting to the assistant…",
  reconnecting: "Reconnecting…",
  listening: "Your turn",
  speaking: "The assistant is speaking…",
  liveHint: "Confirm the recap to the assistant to send the ticket.",
  resumeFailed: "Reconnected; the assistant picked up where you left off.",
  rateLimited: "Too many sessions in a short time. Wait a minute, then try again.",
  tokenError: "Couldn't open a voice session. Try again in a moment.",
  connectError: "Couldn't reach the voice assistant. Check the connection, then try again.",
  timeout: "The voice assistant isn't answering. Try again.",
  closed: "The connection was lost.",
  closedStatus: "Connection lost",
  closedHelp: "Tap the mic to carry on.",
  retry: "Tap the mic to try again",
  unavailable: "Unavailable",
  paused: "Conversation paused",
  submitting: "Sending the ticket…",
  submitError: "Sending the ticket failed. The assistant will offer to try again.",
  finishing: "Ticket sent. The assistant is wrapping up…",
  asking: "Checking with Sky…",
  askLabel: "Question for Sky",
  askPending: "Waiting for the answer…",
  askAnswered: "Answered",
  askFailed: "No answer",
  live: "Live.",
  details: "Technical details",
  published: "Ticket sent",
  publishedIn: (room) => `Ticket sent to “${room}”: Sky files it and confirms in the room.`
}

// Button labels (the round button's accessible name).
const TOGGLE_LABELS = {
  idle: "Start", starting: "Connecting…", live: "Hang up", finishing: "Hang up",
  stopped: "Resume", closed: "Resume", error: "Try again", unavailable: "Unavailable", done: "Done"
}

// ---------------------------------------------------------------------------------------------
// Pure helpers

export function base64FromBytes(bytes) {
  if (!(bytes instanceof Uint8Array)) bytes = new Uint8Array(bytes)
  if (typeof bytes.toBase64 === "function") return bytes.toBase64()

  let binary = ""
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000))
  }
  return btoa(binary)
}

export function bytesFromBase64(base64) {
  if (typeof Uint8Array.fromBase64 === "function") return Uint8Array.fromBase64(base64)

  const binary = atob(base64)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return bytes
}

// Int16 little-endian PCM → Float32 samples in [-1, 1).
export function float32FromPcm16(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const samples = new Float32Array(bytes.byteLength >> 1)
  for (let i = 0; i < samples.length; i++) samples[i] = view.getInt16(i * 2, true) / 0x8000
  return samples
}

// Server frames can be text or binary (JSON as UTF-8 bytes). Returns a string, or a Promise of
// one for a Blob.
export function frameText(data) {
  if (typeof data === "string") return data
  if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) return new TextDecoder().decode(data)
  if (data && typeof data.text === "function") return data.text()
  return ""
}

export function setupMessage(handle) {
  return handle ? { setup: { sessionResumption: { handle } } } : { setup: {} }
}

export function audioMessage(base64) {
  return { realtimeInput: { audio: { data: base64, mimeType: INPUT_MIME } } }
}

export function textMessage(text) {
  return { realtimeInput: { text } }
}

export function toolResponseMessage(responses) {
  return { toolResponse: { functionResponses: responses } }
}

// Accumulates the incremental transcriptions into alternating "Employee: …" / "Assistant: …"
// turns.
export class Transcript {
  constructor() {
    this.lines = []
    this.turnEnded = false
  }

  // Returns the line that was appended to or created.
  append(role, text) {
    if (!text) return null

    const last = this.lines[this.lines.length - 1]
    const continues = last && last.role === role && !(role === "model" && this.turnEnded)
    this.turnEnded = false

    if (continues) {
      last.text += text
      return last
    } else {
      const line = { role, text: text.replace(/^\s+/, "") }
      this.lines.push(line)
      return line
    }
  }

  // The model's next words start a new line even if the employee said nothing in between.
  endTurn() {
    this.turnEnded = true
  }

  get empty() {
    return this.toString() === ""
  }

  toString() {
    return this.lines
      .map(({ role, text }) => `${LABELS[role]}: ${text.replace(/\s+/g, " ").trim()}`)
      .filter(line => !line.endsWith(": "))
      .join("\n")
  }
}

// Sent as text after a reconnection. Observed on 2026-09-29: a resumed session is accepted, but
// the server only issued a resumption handle right after setup, so the conversation since then
// was lost. Replaying the transcript makes reconnects safe either way.
export function recapText(transcript) {
  return "[Resuming after a connection loss. Here is the conversation so far; do not repeat it " +
    "and do not ask again what was already answered: carry on where it stopped, in the language " +
    "the employee was speaking.]\n" + transcript.toString()
}

// ---------------------------------------------------------------------------------------------
// Protocol

export class TokenError extends Error {
  constructor(status) {
    super(`token request failed (${status})`)
    this.status = status
  }
}

export class ConnectError extends Error {
  constructor(code, reason) {
    super(`socket closed before setup completed (${code}${reason ? `: ${reason}` : ""})`)
    this.code = code
    this.reason = reason
  }
}

// One logical conversation with Gemini Live, possibly spanning several WebSocket connections
// (goAway / resumption). Callbacks (all optional):
//   onAudio(base64)                 24 kHz Int16LE PCM to play
//   onInterrupted()                 user barged in: flush queued playback
//   onTranscript(role, text)        role is "user" or "model"; text is an increment
//   onTurnComplete(), onGenerationComplete()
//   onToolCall({ id, name, args })  → Promise<response object>; sent back as the toolResponse
//   onReconnecting(), onReconnected({ resumed })
//   onClosed({ code, reason })      unexpected close of the live connection
export class LiveSession {
  constructor({ fetchToken, createSocket = url => new WebSocket(url), handlers = {} }) {
    this.fetchToken = fetchToken
    this.createSocket = createSocket
    this.handlers = handlers
    this.handle = null
    this.socket = null
    this.ready = false
    this.closed = false
    this.reconnecting = false
    this.cancelledCalls = new Set()
  }

  async start() {
    this.closed = false
    const socket = await this.#open(null)
    if (this.closed) {
      closeQuietly(socket)
      throw new Error("session closed while connecting")
    }
    this.#replaceSocket(socket)
    return { resumed: false }
  }

  // Reopens after an unexpected close, resuming if a handle is known. Resolves { resumed }.
  async restart() {
    this.closed = false
    return (await this.#reconnect()) || { resumed: true }
  }

  sendAudio(base64) {
    return this.#send(audioMessage(base64))
  }

  sendText(text) {
    return this.#send(textMessage(text))
  }

  close() {
    this.closed = true
    this.ready = false
    const socket = this.socket
    this.socket = null
    if (socket) closeQuietly(socket)
  }

  // Dispatches one decoded server message from the current connection.
  handleServerMessage(message) {
    if (!message || typeof message !== "object") return

    const { serverContent, toolCall, toolCallCancellation, sessionResumptionUpdate, goAway } = message

    if (serverContent) this.#handleServerContent(serverContent)

    if (toolCall) {
      for (const call of toolCall.functionCalls || []) this.#runTool(call)
    }

    if (toolCallCancellation) {
      for (const id of toolCallCancellation.ids || []) this.cancelledCalls.add(id)
    }

    if (sessionResumptionUpdate) {
      const { newHandle, resumable } = sessionResumptionUpdate
      if (newHandle && resumable !== false) this.handle = newHandle
    }

    if (goAway) {
      this.#reconnect().catch(error => {
        // The old connection is still serving if it hasn't closed yet; its close reports the loss.
        if (!this.socket && !this.closed) this.handlers.onClosed?.({ code: "reconnect", reason: error.message })
      })
    }
  }

  #handleServerContent(content) {
    const handlers = this.handlers

    if (content.interrupted) handlers.onInterrupted?.()

    if (content.inputTranscription?.text) handlers.onTranscript?.("user", content.inputTranscription.text)

    for (const part of content.modelTurn?.parts || []) {
      const data = part.inlineData?.data
      if (data && (!part.inlineData.mimeType || part.inlineData.mimeType.startsWith("audio/"))) {
        handlers.onAudio?.(data)
      }
    }

    if (content.outputTranscription?.text) handlers.onTranscript?.("model", content.outputTranscription.text)

    if (content.generationComplete) handlers.onGenerationComplete?.()
    if (content.turnComplete) handlers.onTurnComplete?.()
  }

  async #runTool({ id, name, args }) {
    let response
    try {
      response = this.handlers.onToolCall
        ? await this.handlers.onToolCall({ id, name, args: args || {} })
        : { error: `Unknown tool: ${name}` }
    } catch (error) {
      response = { error: String(error?.message || error) }
    }

    if (this.cancelledCalls.has(id)) {
      this.cancelledCalls.delete(id)
    } else {
      this.#send(toolResponseMessage([ { id, name, response } ]))
    }
  }

  // Swaps to a fresh connection (fresh token), resuming the conversation if a handle is known and
  // falling back to a new conversation if the resumption is refused. The old connection keeps
  // serving until the new one is set up.
  async #reconnect() {
    if (this.reconnecting || this.closed) return
    this.reconnecting = true
    this.handlers.onReconnecting?.()

    try {
      let resumed = false
      let socket

      if (this.handle) {
        try {
          socket = await this.#open(this.handle)
          resumed = true
        } catch (error) {
          if (error instanceof TokenError) throw error
          this.handle = null
        }
      }
      socket ||= await this.#open(null)

      if (this.closed) {
        closeQuietly(socket)
      } else {
        this.#replaceSocket(socket)
        this.handlers.onReconnected?.({ resumed })
      }
      return { resumed }
    } finally {
      this.reconnecting = false
    }
  }

  #replaceSocket(socket) {
    const previous = this.socket
    this.socket = socket
    this.ready = true
    if (previous && previous !== socket) closeQuietly(previous)
  }

  // Opens a connection with a fresh single-use token and resolves with the socket once the server
  // has acknowledged the setup. Messages are decoded in order through a per-socket queue and only
  // dispatched while that socket is the current one.
  async #open(handle) {
    const { token, ws_url } = await withTimeout(this.fetchToken(), STEP_TIMEOUT_MS.token, "token")
    const socket = this.createSocket(`${ws_url}?access_token=${encodeURIComponent(token)}`)
    socket.binaryType = "arraybuffer"

    return new Promise((resolve, reject) => {
      let settled = false
      const deadline = setTimeout(() => {
        if (settled) return
        settled = true
        closeQuietly(socket)
        reject(new StepTimeout("connect"))
      }, STEP_TIMEOUT_MS.connect)
      let inbox = Promise.resolve()

      socket.onopen = () => socket.send(JSON.stringify(setupMessage(handle)))

      socket.onmessage = ({ data }) => {
        inbox = inbox.then(() => frameText(data)).then(text => {
          let message
          try { message = JSON.parse(text) } catch { return }

          if (!settled) {
            if (message.setupComplete) {
              settled = true
              clearTimeout(deadline)
              resolve(socket)
            }
          } else if (socket === this.socket) {
            this.handleServerMessage(message)
          }
        }).catch(error => console.error("voice: message handling failed", error))
      }

      socket.onerror = () => {}

      socket.onclose = ({ code, reason }) => {
        if (!settled) {
          settled = true
          clearTimeout(deadline)
          reject(new ConnectError(code, reason))
        } else if (socket === this.socket && !this.closed) {
          this.socket = null
          this.ready = false
          if (!this.reconnecting) this.handlers.onClosed?.({ code, reason })
        }
      }
    })
  }

  #send(message) {
    const socket = this.socket
    if (this.ready && socket && socket.readyState === 1) {
      socket.send(JSON.stringify(message))
      return true
    }
    return false
  }
}

function closeQuietly(socket) {
  try { socket.close(1000) } catch {}
}

// ---------------------------------------------------------------------------------------------
// Audio

// Gapless playback of 24 kHz chunks on the shared AudioContext's timeline. `onActivity(true)`
// when audio starts playing, `onActivity(false)` once everything queued has played (or was
// flushed): the page's "L'assistant parle…".
class Player {
  constructor(audioContext, onActivity = () => {}) {
    this.audioContext = audioContext
    this.onActivity = onActivity
    this.cursor = 0
    this.sources = new Set()
  }

  enqueue(base64) {
    const samples = float32FromPcm16(bytesFromBase64(base64))
    if (samples.length === 0) return

    const buffer = this.audioContext.createBuffer(1, samples.length, OUTPUT_RATE)
    buffer.copyToChannel(samples, 0)

    const source = this.audioContext.createBufferSource()
    source.buffer = buffer
    source.connect(this.audioContext.destination)
    source.onended = () => {
      this.sources.delete(source)
      if (this.sources.size === 0) this.onActivity(false)
    }

    const startAt = Math.max(this.cursor, this.audioContext.currentTime + PLAYBACK_LEAD_S)
    source.start(startAt)
    this.cursor = startAt + buffer.duration
    const wasIdle = this.sources.size === 0
    this.sources.add(source)
    if (wasIdle) this.onActivity(true)
  }

  flush() {
    const hadAudio = this.sources.size > 0
    for (const source of this.sources) {
      source.onended = null
      try { source.stop() } catch {}
      source.disconnect()
    }
    this.sources.clear()
    this.cursor = 0
    if (hadAudio) this.onActivity(false)
  }

  get idle() {
    return this.sources.size === 0
  }
}

// RMS of a chunk of Int16 PCM, scaled so that speech fills most of the ring (0 … 1).
export function levelFromPcm16(buffer) {
  const samples = new Int16Array(buffer)
  if (samples.length === 0) return 0
  let sum = 0
  for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i]
  return Math.min(1, Math.sqrt(sum / samples.length) / 0x8000 * 5)
}

const pad = (n) => String(n).padStart(2, "0")

// For display: typographic apostrophes. Nothing language-specific (the transcript is in whatever
// language the employee speaks).
export function displayText(text) {
  return text.replace(/'/g, "’")
}

// 65 → "01:05"
export function formatClock(seconds) {
  const whole = Math.max(0, Math.floor(seconds))
  return `${pad(Math.floor(whole / 60))}:${pad(whole % 60)}`
}

// ---------------------------------------------------------------------------------------------
// Stimulus controller
//
// States (data-voice-state): idle → starting → live ⇄ (finishing → done)
//   live → stopped ("Hang up"), closed (connection lost), error; each can resume (retry())
//   with the transcript kept, or "Start over" (in-page confirmation) wipes it.
//   unavailable: no HTTPS, no AudioWorklet.
// While live, data-voice-activity is "speaking" while the assistant's audio plays, else
// "listening".

export default class extends Controller {
  static targets = [ "toggle", "label", "control", "status", "timer", "hint", "transcript", "notice", "noticeBody",
    "confirm", "cancel", "resume", "restart", "result", "resultText", "messageLink", "announcer" ]
  static values = { tokenUrl: String, reportUrl: String, askUrl: String, workletUrl: String, roomUrl: String, roomName: String }

  connect() {
    this.state = "idle"
    this.activity = "listening"
    this.transcript = new Transcript()
    this.submitted = false
    this.liveSeconds = 0
    this.liveSince = null
    this.level = 0
    this.onVisibilityChange = () => this.#reacquireWakeLock()
    document.addEventListener("visibilitychange", this.onVisibilityChange)

    // The transcript sticks to its newest line unless scrolled up, also when the notice or the
    // action buttons appear and shrink it.
    this.stickToBottom = true
    this.onTranscriptScroll = () => {
      const t = this.transcriptTarget
      this.stickToBottom = t.scrollHeight - t.scrollTop - t.clientHeight < 48
    }
    this.transcriptTarget.addEventListener("scroll", this.onTranscriptScroll, { passive: true })
    if (window.ResizeObserver) {
      this.transcriptObserver = new ResizeObserver(() => this.#stickTranscript())
      this.transcriptObserver.observe(this.transcriptTarget)
    }

    if (!window.isSecureContext) {
      this.#fail({ message: MESSAGES.insecure, retry: false })
    } else if (!navigator.mediaDevices?.getUserMedia || !window.AudioWorkletNode) {
      this.#fail({ message: MESSAGES.unsupported, retry: false })
    } else {
      this.#render()
    }
  }

  disconnect() {
    document.removeEventListener("visibilitychange", this.onVisibilityChange)
    this.transcriptTarget.removeEventListener("scroll", this.onTranscriptScroll)
    this.transcriptObserver?.disconnect()
    this.#teardown()
    this.session = null
    clearInterval(this.clockTimer)
    clearTimeout(this.transientTimer)
  }

  // Works whether or not the markup wires the button with data-action="voice#toggle".
  toggleTargetConnected(button) {
    if (!(button.dataset.action || "").includes("voice#")) {
      button.addEventListener("click", event => this.toggle(event))
    }
  }

  toggle(event) {
    event?.preventDefault()

    switch (this.state) {
      case "idle":      return this.start()
      case "live":
      case "finishing": return this.stop()
      case "stopped":
      case "closed":
      case "error":     return this.retry()
    }
  }

  // A fresh conversation. Only reached with an empty transcript (idle, or after "Start over").
  async start() {
    if (this.state === "starting" || this.state === "live") return
    if (!this.#supported) return

    this.#reset()
    this.#enterStarting(MESSAGES.stepMic)

    try {
      await this.#startAudio()
      this.#setStep(MESSAGES.stepConnect)
      this.session = this.#buildSession()
      await this.session.start()
      if (this.state !== "starting") return this.session?.close()

      this.session.sendText(KICKOFF_TEXT)
      this.#enterLive()
    } catch (error) {
      if (this.state === "starting") this.#startFailed(error)
    }
  }

  // Resumes the conversation (after "Hang up", a lost connection or an error), transcript
  // kept: a fresh token, the session resumed when possible, and the transcript replayed to the
  // assistant either way (LiveSession's onReconnected).
  async retry() {
    if (!this.session) return this.start()
    if (this.state === "starting" || this.state === "live") return

    this.#hideNotice()
    this.#hideConfirm()
    this.#enterStarting(this.audioContext ? MESSAGES.reconnecting : MESSAGES.stepMic)

    try {
      if (!this.audioContext || this.audioContext.state === "closed") await this.#startAudio()
      await withTimeout(this.audioContext.resume().catch(() => {}), 3000, "audio").catch(() => {})
      this.#setStep(MESSAGES.stepConnect)
      const { resumed } = await this.session.restart()
      if (this.state !== "starting") return

      this.#enterLive()
      if (!resumed) this.#setTransient(MESSAGES.resumeFailed, 4000)
    } catch (error) {
      if (this.state === "starting") this.#startFailed(error)
    }
  }

  // "Hang up": ends the call, keeps the transcript (and the session, to resume).
  stop() {
    if (this.state === "finishing") return this.#finish()
    this.#teardown()
    this.#hideNotice()
    this.state = this.submitted ? "done" : (this.transcript.empty ? "idle" : "stopped")
    this.#render()
    this.#announce(this.state === "stopped" ? MESSAGES.paused : "")
  }

  // The small "Cancel" while starting.
  cancel(event) {
    event?.preventDefault()
    if (this.state !== "starting") return
    this.#teardown()
    this.state = this.transcript.empty ? "idle" : "stopped"
    this.#render()
    this.#focus(this.toggleTarget)
  }

  // "Start over": asks first when there's something to lose.
  restart(event) {
    event?.preventDefault()
    if (this.transcript.empty) return this.confirmRestart()

    this.confirmTarget.hidden = false
    this.#focus(this.confirmTarget.querySelector("button"))
  }

  confirmRestart(event) {
    event?.preventDefault()
    this.#hideConfirm()
    this.#teardown()
    this.session = null
    this.state = "idle"
    this.start()
  }

  cancelRestart(event) {
    event?.preventDefault()
    this.#hideConfirm()
    this.#focus(this.hasRestartTarget ? this.restartTarget : this.toggleTarget)
  }

  // Session wiring

  #buildSession() {
    return new LiveSession({
      fetchToken: () => this.#fetchToken(),
      handlers: {
        onAudio: data => this.player?.enqueue(data),
        onInterrupted: () => {
          this.player?.flush()
          this.#setActivity("listening")
        },
        onTranscript: (role, text) => this.#appendTranscript(role, text),
        onTurnComplete: () => {
          this.transcript.endTurn()
          this.#completeTurn()
          if (this.player?.idle !== false) this.#setActivity("listening")
          if (this.state === "finishing") this.#finishSoon()
        },
        onToolCall: call => this.#handleToolCall(call),
        onReconnecting: () => { if (this.state === "live") this.#setTransient(MESSAGES.reconnecting) },
        onReconnected: ({ resumed }) => {
          if (!this.submitted) this.session?.sendText(this.transcript.empty ? KICKOFF_TEXT : recapText(this.transcript))
          if (this.state === "live") this.#setTransient(resumed ? null : MESSAGES.resumeFailed, 4000)
        },
        onClosed: ({ code, reason }) => this.#connectionLost(code, reason)
      }
    })
  }

  #handleToolCall({ name, args }) {
    if (name === "submit_incident") return this.#submitIncident(args)
    if (name === "ask_hermes" && this.askUrlValue) return this.#askHermes(args)
    return { error: `Unknown tool: ${name}` }
  }

  // `ask_hermes`: the answer (or an error the assistant tells the employee about) is the tool
  // response. The assistant waits for it (blocking function call), having said it is checking
  // with Sky (in the employee's language).
  async #askHermes(args) {
    const question = String(args?.question || "").trim()
    if (!question) return { error: "Empty question." }

    const line = this.#appendHermesLine(question)
    this.#setTransient(MESSAGES.asking)
    this.#announce(`${MESSAGES.askLabel}: ${question}`)

    try {
      const answer = await this.#postQuestion(question)
      this.#settleHermesLine(line, true)
      return { result: answer }
    } catch (error) {
      console.warn("voice: ask_hermes failed", error)
      this.#settleHermesLine(line, false)
      return { error: "Sky did not answer." }
    } finally {
      if (this.transient === MESSAGES.asking) this.#setTransient(null)
    }
  }

  async #submitIncident(args) {
    if (this.submitted) return { result: "already_submitted" }

    this.#setTransient(MESSAGES.submitting)

    try {
      const report = await this.#postReport(args)
      this.submitted = true
      this.state = "finishing"
      this.#hideNotice()
      this.#showResult(report)
      this.#setTransient(null)
      this.#render()
      this.finishTimer = setTimeout(() => this.#finishSoon(), FINISH_TIMEOUT_MS)
      return { result: "ok" }
    } catch (error) {
      console.warn("voice: report failed", error)
      this.#setTransient(null)
      this.#showNotice({ message: MESSAGES.submitError, details: error.message })
      return { result: "error", error: "The ticket could not be sent. Tell the employee, in their language, and offer to try again." }
    }
  }

  // Lets the assistant finish its confirmation before hanging up.
  #finishSoon() {
    clearTimeout(this.finishTimer)
    const waitForPlayback = () => {
      if (this.state !== "finishing") return
      if (this.player && !this.player.idle) {
        this.finishTimer = setTimeout(waitForPlayback, 250)
      } else {
        this.#finish()
      }
    }
    this.finishTimer = setTimeout(waitForPlayback, 250)
  }

  #finish() {
    if (this.state !== "finishing") return
    this.#teardown()
    this.session = null
    this.state = "done"
    this.#render()
    this.#announce(this.hasResultTextTarget ? this.resultTextTarget.textContent : MESSAGES.published)
    if (this.hasMessageLinkTarget) this.#focus(this.messageLinkTarget)
  }

  #connectionLost(code, reason) {
    if (this.state === "finishing") return this.#finish()
    if (this.state !== "live") return

    console.warn("voice: connection closed", code, reason)
    this.#teardown()
    this.state = "closed"
    this.#showNotice({ message: `${MESSAGES.closed} ${MESSAGES.closedHelp}`, details: `WebSocket closed (code ${code}${reason ? `, ${reason}` : ""})` })
    this.#render()
  }

  #micLost() {
    if (this.state === "finishing") return this.#finish()
    if (this.state !== "live") return

    this.#teardown()
    this.#fail({ message: MESSAGES.micLost })
  }

  #startFailed(error) {
    this.#teardown()
    this.#fail(this.#describe(error))
  }

  // HTTP

  async #fetchToken() {
    let response
    try {
      response = await fetch(this.tokenUrlValue, { method: "POST", credentials: "same-origin", headers: this.#headers })
    } catch {
      throw new TokenError(0)
    }
    if (!response.ok) throw new TokenError(response.status)

    const body = await response.json()
    if (!body?.token || !body?.ws_url) throw new TokenError(response.status)
    return body
  }

  async #postReport(args) {
    const body = JSON.stringify({ ...args, transcript: this.transcript.toString() })
    const response = await fetch(this.reportUrlValue, {
      method: "POST", credentials: "same-origin", headers: this.#headers, body
    })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    return response.json().catch(() => ({}))
  }

  // Resolves with the answer text; rejects on an HTTP error, a network error or ASK_TIMEOUT_MS.
  async #postQuestion(question) {
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), ASK_TIMEOUT_MS)
    try {
      const response = await fetch(this.askUrlValue, {
        method: "POST", credentials: "same-origin", headers: this.#headers,
        body: JSON.stringify({ question }), signal: controller.signal
      })
      if (!response.ok) throw new Error(`HTTP ${response.status}`)
      const body = await response.json()
      const answer = typeof body?.answer === "string" ? body.answer.trim() : ""
      if (!answer) throw new Error("empty answer")
      return answer
    } finally {
      clearTimeout(timer)
    }
  }

  get #headers() {
    const headers = { "Content-Type": "application/json", "Accept": "application/json" }
    const csrf = document.querySelector("meta[name=csrf-token]")?.content
    if (csrf) headers["X-CSRF-Token"] = csrf
    return headers
  }

  // Audio

  async #startAudio() {
    this.audioContext = new AudioContext()
    this.audioContext.resume().catch(() => {})

    try {
      this.stream = await withTimeout(navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }
      }), STEP_TIMEOUT_MS.mic, "mic")
    } catch (error) {
      if (!(error instanceof StepTimeout)) error.micError = true
      throw error
    }

    // Some browsers (iOS Safari) leave resume() pending after the permission prompt: don't block
    // on it, and resume again once the mic is open.
    await withTimeout(this.audioContext.resume().catch(() => {}), 3000, "audio").catch(() => {})
    await withTimeout(this.audioContext.audioWorklet.addModule(this.workletUrlValue), STEP_TIMEOUT_MS.audio, "audio")

    this.source = this.audioContext.createMediaStreamSource(this.stream)
    this.worklet = new AudioWorkletNode(this.audioContext, "pcm-capture", {
      numberOfOutputs: 1,
      processorOptions: { targetRate: INPUT_RATE, chunkMs: 100 }
    })
    this.worklet.port.onmessage = ({ data }) => {
      if (this.state === "live" || this.state === "finishing") {
        this.#showLevel(levelFromPcm16(data))
        this.session?.sendAudio(base64FromBytes(data))
      }
    }
    this.source.connect(this.worklet)
    this.worklet.connect(this.audioContext.destination) // outputs silence; keeps the node pulled everywhere
    this.player = new Player(this.audioContext, playing => this.#setActivity(playing ? "speaking" : "listening"))

    this.stream.getAudioTracks().forEach(track => {
      track.onended = () => this.#micLost()
    })
  }

  // Stops the audio and the connection. The session object (resumption handle) is kept, so
  // retry() can pick the conversation up again.
  #teardown() {
    clearTimeout(this.finishTimer)
    this.session?.close()
    this.player?.flush()
    this.player = null

    if (this.worklet) {
      this.worklet.port.onmessage = null
      try { this.worklet.port.postMessage("stop") } catch {}
      this.worklet.disconnect()
      this.worklet = null
    }
    this.source?.disconnect()
    this.source = null
    this.stream?.getTracks().forEach(track => { track.onended = null; track.stop() })
    this.stream = null
    if (this.audioContext && this.audioContext.state !== "closed") this.audioContext.close().catch(() => {})
    this.audioContext = null

    this.#releaseWakeLock()
    this.#stopClock()
    this.#showLevel(0)
    this.#setActivity("listening")
    this.#markPartial(null)
  }

  // A new conversation: empty transcript, clock at zero, no result.
  #reset() {
    this.session = null
    this.submitted = false
    this.transcript = new Transcript()
    this.lastLine = null
    this.lastLineElement = null
    this.transcriptTarget.replaceChildren()
    this.stickToBottom = true
    this.liveSeconds = 0
    this.resultTarget.hidden = true
    this.#hideNotice()
    this.#hideConfirm()
  }

  // Wake lock: the screen going to sleep cuts the microphone on phones.

  async #acquireWakeLock() {
    if (!navigator.wakeLock || this.wakeLock || document.visibilityState !== "visible") return
    try {
      this.wakeLock = await navigator.wakeLock.request("screen")
      this.wakeLock.addEventListener("release", () => { this.wakeLock = null })
    } catch {}
  }

  #reacquireWakeLock() {
    if (document.visibilityState === "visible" && (this.state === "live" || this.state === "finishing")) {
      this.#acquireWakeLock()
      this.audioContext?.resume().catch(() => {})
    }
  }

  #releaseWakeLock() {
    this.wakeLock?.release().catch(() => {})
    this.wakeLock = null
  }

  // States

  #enterStarting(step) {
    this.state = "starting"
    this.step = step
    this.#hideNotice()
    this.#render()
    this.#announce(step)
    if (this.hasCancelTarget && (document.activeElement === this.toggleTarget || document.activeElement === document.body)) {
      this.#focus(this.cancelTarget)
    }
  }

  #setStep(step) {
    if (this.state !== "starting") return
    this.step = step
    this.#render()
    this.#announce(step)
  }

  #enterLive() {
    this.state = "live"
    this.#startClock()
    this.#render()
    this.#announce(`${MESSAGES.live} ${MESSAGES.liveHint}`)
    this.#acquireWakeLock()
    if (!this.toggleTarget.contains(document.activeElement)) this.#focus(this.toggleTarget)
  }

  #fail({ message, help, details, retry = true }) {
    this.state = retry ? "error" : "unavailable"
    this.#showNotice({ message, help, details })
    this.#render()
  }

  #setActivity(activity) {
    if (this.activity === activity) return
    this.activity = activity
    this.element.dataset.voiceActivity = activity
    if (this.state === "live") this.#renderStatus()
  }

  // A status that wins over "Your turn" / "The assistant is speaking…" for a while (or until cleared).
  #setTransient(text, ms) {
    clearTimeout(this.transientTimer)
    this.transient = text
    if (text && ms) this.transientTimer = setTimeout(() => this.#setTransient(null), ms)
    this.#renderStatus()
  }

  // Clock (time spent live, across resumptions)

  #startClock() {
    this.liveSince = performance.now()
    clearInterval(this.clockTimer)
    this.clockTimer = setInterval(() => this.#renderClock(), 500)
    this.#renderClock()
  }

  #stopClock() {
    if (this.liveSince !== null) this.liveSeconds += (performance.now() - this.liveSince) / 1000
    this.liveSince = null
    clearInterval(this.clockTimer)
    this.#renderClock()
  }

  get #elapsed() {
    return this.liveSeconds + (this.liveSince === null ? 0 : (performance.now() - this.liveSince) / 1000)
  }

  // Page

  #appendTranscript(role, text) {
    const line = this.transcript.append(role, text)
    if (!line) return

    const container = this.transcriptTarget

    if (line !== this.lastLine) {
      this.lastLine = line
      const element = document.createElement("div")
      element.className = `voice__line voice__line--${role}`
      const label = document.createElement("span")
      label.className = "voice__role"
      label.textContent = SCREEN_LABELS[role]
      const bubble = document.createElement("p")
      bubble.className = "voice__bubble"
      this.lastLineText = document.createTextNode("")
      bubble.append(this.lastLineText)
      element.append(label, bubble)
      container.append(element)
      this.lastLineElement = element
      this.announcedLine = null
    }
    this.lastLineText.data = displayText(line.text)
    this.#markPartial(this.lastLineElement)
    this.#stickTranscript()
  }

  // A "Sky" line (an ask_hermes question) in the transcript: the question, and below it where the answer stands. Not
  // part of `this.transcript` (so not in the report); the assistant's next words start a new
  // bubble below it.
  #appendHermesLine(question) {
    this.transcript.endTurn()
    this.#markPartial(null)
    this.lastLine = null

    const element = document.createElement("div")
    element.className = "voice__line voice__line--hermes voice__line--pending"
    const label = document.createElement("span")
    label.className = "voice__role"
    label.textContent = MESSAGES.askLabel
    const bubble = document.createElement("p")
    bubble.className = "voice__bubble"
    bubble.textContent = displayText(question)
    const state = document.createElement("span")
    state.className = "voice__hermes-state txt-small"
    state.textContent = MESSAGES.askPending
    element.append(label, bubble, state)
    this.transcriptTarget.append(element)
    this.#stickTranscript()
    return element
  }

  #settleHermesLine(element, answered) {
    element.classList.remove("voice__line--pending")
    element.classList.toggle("voice__line--failed", !answered)
    const state = element.querySelector(".voice__hermes-state")
    if (state) state.textContent = answered ? MESSAGES.askAnswered : MESSAGES.askFailed
  }

  #stickTranscript() {
    if (this.stickToBottom) this.transcriptTarget.scrollTop = this.transcriptTarget.scrollHeight
  }

  // The line being streamed; null when nothing is.
  #markPartial(element) {
    if (this.partialElement && this.partialElement !== element) this.partialElement.classList.remove("voice__line--partial")
    this.partialElement = element
    element?.classList.add("voice__line--partial")
  }

  // The assistant's turn is over: its line is final, and read out once to screen readers.
  #completeTurn() {
    const line = this.lastLine
    if (line?.role !== "model") return
    this.#markPartial(null)
    if (this.announcedLine !== line) {
      this.announcedLine = line
      this.#announce(`${SCREEN_LABELS.model}: ${displayText(line.text.trim())}`)
    }
  }

  #showLevel(level) {
    this.level = level === 0 || level > this.level ? level : this.level * 0.6 + level * 0.4
    if (this.hasControlTarget) this.controlTarget.style.setProperty("--voice-level", this.level.toFixed(2))
  }

  #showResult({ message_url }) {
    if (this.hasResultTextTarget) {
      this.resultTextTarget.textContent = this.roomNameValue ? MESSAGES.publishedIn(this.roomNameValue) : `${MESSAGES.published}.`
    }
    if (this.hasMessageLinkTarget) {
      this.messageLinkTarget.href = message_url || this.roomUrlValue
    }
  }

  #showNotice({ message, help, details }) {
    if (details) console.warn("voice:", message, details)
    if (!this.hasNoticeTarget) return this.#renderStatus(message)

    const parts = []
    const sentence = document.createElement("p")
    sentence.textContent = message
    parts.push(sentence)

    if (help) {
      const how = document.createElement("p")
      how.className = "txt-small voice__help"
      how.textContent = help
      parts.push(how)
    }

    if (details) {
      const disclosure = document.createElement("details")
      const summary = document.createElement("summary")
      summary.textContent = MESSAGES.details
      const code = document.createElement("code")
      code.textContent = details
      disclosure.append(summary, code)
      parts.push(disclosure)
    }

    const body = this.hasNoticeBodyTarget ? this.noticeBodyTarget : this.noticeTarget
    body.replaceChildren(...parts)
    this.noticeTarget.hidden = false
  }

  #hideNotice() {
    if (!this.hasNoticeTarget) return
    this.noticeTarget.hidden = true
    if (this.hasNoticeBodyTarget) this.noticeBodyTarget.replaceChildren()
  }

  #hideConfirm() {
    if (this.hasConfirmTarget) this.confirmTarget.hidden = true
  }

  #announce(text) {
    if (!this.hasAnnouncerTarget || !text) return
    // Cleared first so the same sentence twice is still read.
    this.announcerTarget.textContent = ""
    requestAnimationFrame(() => { this.announcerTarget.textContent = text })
  }

  #focus(element) {
    try { element?.focus({ preventScroll: true }) } catch {}
  }

  #render() {
    const state = this.state
    const live = state === "live" || state === "finishing"
    this.element.dataset.voiceState = state
    this.element.dataset.voiceActivity = this.activity

    if (this.hasToggleTarget) {
      const button = this.toggleTarget
      button.classList.toggle("btn--negative", live)
      button.classList.toggle("btn--reversed", !live)
      button.disabled = state === "starting" || state === "unavailable" || state === "done"
      button.hidden = state === "done"
      button.title = TOGGLE_LABELS[state] || ""
      if (this.hasLabelTarget) this.labelTarget.textContent = TOGGLE_LABELS[state] || ""
    }
    if (this.hasControlTarget) {
      if (state === "starting") {
        this.controlTarget.setAttribute("aria-busy", "true")
      } else {
        this.controlTarget.removeAttribute("aria-busy")
      }
    }

    const resumable = state === "stopped" || state === "closed" || (state === "error" && !this.transcript.empty)
    if (this.hasCancelTarget) this.cancelTarget.hidden = state !== "starting"
    if (this.hasResumeTarget) this.resumeTarget.hidden = !resumable
    if (this.hasRestartTarget) this.restartTarget.hidden = !(resumable || state === "error") || this.transcript.empty
    if (this.hasResultTarget) this.resultTarget.hidden = state !== "done"
    if (this.hasHintTarget) this.hintTarget.textContent = live && state === "live" ? MESSAGES.liveHint : ""

    this.#renderStatus()
    this.#renderClock()
  }

  #renderStatus(override) {
    if (!this.hasStatusTarget) return
    const texts = {
      idle: MESSAGES.ready,
      starting: this.step,
      live: this.transient || (this.activity === "speaking" ? MESSAGES.speaking : MESSAGES.listening),
      finishing: MESSAGES.finishing,
      stopped: MESSAGES.paused,
      closed: MESSAGES.closedStatus,
      error: MESSAGES.retry,
      unavailable: MESSAGES.unavailable,
      done: MESSAGES.published
    }
    const text = override || texts[this.state] || ""
    if (this.statusTarget.textContent !== text) this.statusTarget.textContent = text
  }

  #renderClock() {
    if (!this.hasTimerTarget) return
    const elapsed = this.#elapsed
    this.timerTarget.hidden = elapsed < 0.5 && this.state !== "live"
    const text = formatClock(elapsed)
    if (this.timerTarget.textContent !== text) this.timerTarget.textContent = text
  }

  // The sentence stays plain; codes and hostnames go in "Technical details".
  #describe(error) {
    if (error?.micError) {
      const details = [ error.name, error.message ].filter(Boolean).join(" : ")
      switch (error.name) {
        case "NotAllowedError":
        case "PermissionDeniedError":
        case "SecurityError":        return { message: MESSAGES.micDenied, help: MESSAGES.micDeniedHelp, details }
        case "NotFoundError":
        case "DevicesNotFoundError":
        case "OverconstrainedError": return { message: MESSAGES.micMissing, details }
        default:                     return { message: MESSAGES.micError, details }
      }
    }
    if (error instanceof StepTimeout) {
      return { message: error.step === "mic" ? MESSAGES.micError : MESSAGES.timeout, details: `Timed out at step “${STEP_LABELS[error.step] || error.step}”` }
    }
    if (error instanceof TokenError) {
      const details = error.status ? `POST ${this.tokenUrlValue} → HTTP ${error.status}` : `POST ${this.tokenUrlValue}: network unreachable`
      return { message: error.status === 429 ? MESSAGES.rateLimited : MESSAGES.tokenError, details }
    }
    if (error instanceof ConnectError) {
      return { message: MESSAGES.connectError, details: `generativelanguage.googleapis.com: WebSocket closed (code ${error.code}${error.reason ? `, ${error.reason}` : ""})` }
    }
    return { message: MESSAGES.connectError, details: String(error?.message || error) }
  }

  get #supported() {
    return window.isSecureContext && navigator.mediaDevices?.getUserMedia && window.AudioWorkletNode
  }
}
