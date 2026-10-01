// Hermes fork: one Gemini Live conversation from the browser, shared by the live voice report
// (controllers/voice_controller.js) and Sky push-to-talk (hermes/sky_ptt.js); docs/hermes-gemini-live.md.
// Moved out of voice_controller.js in Sky batch 1a, unchanged for the voice page, plus what
// push-to-talk needs: manual activity detection (`activityStart` / `activityEnd`), the screen note
// before a turn (`clientContent`, `turnComplete: false`), `usageMetadata` and Player's options.
//
// No DOM and no Stimulus: Node's test runner exercises it with a fake socket
// (`node --test crates/assets/tests/js/*.test.mjs`). Pinned as `lib/hermes/live_session` by the
// import map's `pin_all_from "app/javascript/lib"`.

export const INPUT_RATE = 16000
export const OUTPUT_RATE = 24000
const INPUT_MIME = `audio/pcm;rate=${INPUT_RATE}`
const PLAYBACK_LEAD_S = 0.05

// Every start-up step has a deadline, so a stalled step ends with a message naming it instead of
// an endless "Connecting…".
export const STEP_TIMEOUT_MS = { mic: 60000, audio: 10000, token: 15000, connect: 20000 }

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


export function activityStartMessage() {
  return { realtimeInput: { activityStart: {} } }
}

export function activityEndMessage() {
  return { realtimeInput: { activityEnd: {} } }
}

// Text the model reads before the next turn without answering it (push-to-talk's screen note):
// a user turn in the conversation, left open.
export function contextMessage(text) {
  return { clientContent: { turns: [ { role: "user", parts: [ { text } ] } ], turnComplete: false } }
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
//   onUsage(usageMetadata)          token counts, when the server sends them
// `fetchToken({ resume })` is called for each connection; `resume` is true when reconnecting with a
// resumption handle (push-to-talk's server counts those at a quarter).
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

  // Push-to-talk (manual activity detection): the screen note, if any, then the start of the
  // person's turn; their audio follows. False when the connection isn't ready.
  beginTurn(note) {
    if (note && !this.#send(contextMessage(note))) return false
    return this.#send(activityStartMessage())
  }

  // The end of the person's turn: the model answers.
  endTurn() {
    return this.#send(activityEndMessage())
  }

  // Text the model reads without answering it at once (a note between turns).
  sendContext(text) {
    return this.#send(contextMessage(text))
  }

  get open() {
    return Boolean(this.ready && this.socket && this.socket.readyState === 1)
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

    const { serverContent, toolCall, toolCallCancellation, sessionResumptionUpdate, goAway, usageMetadata } = message

    if (serverContent) this.#handleServerContent(serverContent)

    if (usageMetadata) this.handlers.onUsage?.(usageMetadata)

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
    const { token, ws_url } = await withTimeout(this.fetchToken({ resume: Boolean(handle) }), STEP_TIMEOUT_MS.token, "token")
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
export class Player {
  constructor(audioContext, onActivity = () => {}, destination = null) {
    this.audioContext = audioContext
    this.onActivity = onActivity
    this.destination = destination
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
    source.connect(this.destination || this.audioContext.destination)
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
