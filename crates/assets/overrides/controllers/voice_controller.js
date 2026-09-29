import { Controller } from "@hotwired/stimulus"

// Live voice incident report over Gemini Live, spoken directly from the browser.
//
// The server mints a single-use ephemeral token that locks the whole session setup (model,
// instructions, tools, transcription, resumption, compression), so the browser only ever sends
// `{"setup":{}}` (plus a resumption handle when reconnecting) and never sees the API key. The
// microphone is captured by voice/pcm-worklet.js (16 kHz Int16LE mono, ~100 ms chunks); the
// model answers with 24 kHz Int16LE mono audio and incremental transcriptions of both sides.
// When the model calls `submit_incident`, its arguments plus the accumulated transcript are
// POSTed to the report URL, which publishes the message in the room as the current user.
//
// The protocol lives in LiveSession (no DOM, no audio) so it can be exercised on its own; the
// Stimulus controller wires it to the microphone, the speaker and the page.

const INPUT_RATE = 16000
const OUTPUT_RATE = 24000
const INPUT_MIME = `audio/pcm;rate=${INPUT_RATE}`
const FINISH_TIMEOUT_MS = 10000
const PLAYBACK_LEAD_S = 0.05

const LABELS = { user: "Employé", model: "Assistant" }

// Every start-up step has a deadline, so a stalled step ends with a message naming it instead of
// an endless "Connexion en cours…".
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

const STEP_LABELS = {
  mic: "autorisation du micro",
  audio: "démarrage de l'audio du navigateur",
  token: "ouverture de session sur le serveur Campfire",
  connect: "connexion au service vocal Google (generativelanguage.googleapis.com)"
}

export const MESSAGES = {
  idle: "Appuyez sur « Démarrer » puis décrivez l'incident à voix haute.",
  insecure: "Le micro n'est disponible qu'en HTTPS : ouvrez cette page via une adresse https://.",
  unsupported: "Ce navigateur ne permet pas la capture audio en direct (AudioWorklet). Essayez un navigateur récent.",
  micDenied: "Accès au micro refusé. Autorisez le micro pour ce site dans les réglages du navigateur, puis réessayez.",
  micMissing: "Aucun micro détecté sur cet appareil.",
  micError: "Impossible d'ouvrir le micro.",
  starting: "Connexion en cours…",
  live: "En direct — parlez, l'assistant vous écoute.",
  rateLimited: "Trop de sessions demandées en peu de temps. Patientez une minute puis réessayez.",
  tokenError: "Impossible d'ouvrir une session vocale (erreur serveur). Réessayez dans un instant.",
  connectError: "Impossible de joindre le service vocal. Vérifiez la connexion puis réessayez.",
  reconnecting: "Reconnexion en cours…",
  resumeFailed: "Connexion rétablie (nouvelle session, l'assistant a reçu le fil de la conversation) — continuez.",
  closed: "La connexion a été interrompue.",
  submitting: "Envoi du compte rendu…",
  submitted: "Compte rendu publié.",
  submitError: "L'envoi du compte rendu a échoué.",
  stopped: "Session terminée."
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

// Accumulates the incremental transcriptions into alternating "Employé : …" / "Assistant : …"
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
      .map(({ role, text }) => `${LABELS[role]} : ${text.replace(/\s+/g, " ").trim()}`)
      .filter(line => !line.endsWith(": "))
      .join("\n")
  }
}

// Sent as text after a reconnection. Observed on 2026-09-29: a resumed session is accepted, but
// the server only issued a resumption handle right after setup, so the conversation since then
// was lost. Replaying the transcript makes reconnects safe either way.
export function recapText(transcript) {
  return "[Reprise après une coupure de connexion. Voici la conversation jusqu'ici ; " +
    "ne la répète pas et ne pose pas de nouveau les questions déjà traitées : " +
    "continue là où elle s'est arrêtée.]\n" + transcript.toString()
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
        : { error: `Outil inconnu : ${name}` }
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

// Gapless playback of 24 kHz chunks on the shared AudioContext's timeline.
class Player {
  constructor(context) {
    this.context = context
    this.cursor = 0
    this.sources = new Set()
  }

  enqueue(base64) {
    const samples = float32FromPcm16(bytesFromBase64(base64))
    if (samples.length === 0) return

    const buffer = this.context.createBuffer(1, samples.length, OUTPUT_RATE)
    buffer.copyToChannel(samples, 0)

    const source = this.context.createBufferSource()
    source.buffer = buffer
    source.connect(this.context.destination)
    source.onended = () => this.sources.delete(source)

    const startAt = Math.max(this.cursor, this.context.currentTime + PLAYBACK_LEAD_S)
    source.start(startAt)
    this.cursor = startAt + buffer.duration
    this.sources.add(source)
  }

  flush() {
    for (const source of this.sources) {
      try { source.stop() } catch {}
      source.disconnect()
    }
    this.sources.clear()
    this.cursor = 0
  }

  get idle() {
    return this.sources.size === 0
  }
}

// ---------------------------------------------------------------------------------------------
// Stimulus controller

export default class extends Controller {
  static targets = [ "toggle", "status", "transcript", "result" ]
  static values = { tokenUrl: String, reportUrl: String, workletUrl: String, roomUrl: String, roomName: String }

  connect() {
    this.state = "idle"
    this.transcript = new Transcript()
    this.submitted = false
    this.onVisibilityChange = () => this.#reacquireWakeLock()
    document.addEventListener("visibilitychange", this.onVisibilityChange)

    if (!window.isSecureContext) {
      this.#fail(MESSAGES.insecure, { retry: false })
    } else if (!navigator.mediaDevices?.getUserMedia || !window.AudioWorkletNode) {
      this.#fail(MESSAGES.unsupported, { retry: false })
    } else {
      this.#setStatus(MESSAGES.idle)
      this.#render()
    }
  }

  disconnect() {
    document.removeEventListener("visibilitychange", this.onVisibilityChange)
    this.#teardown()
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
      case "idle":
      case "done":
      case "error":   return this.start()
      case "closed":  return this.retry()
      case "live":
      case "finishing":
      case "starting": return this.stop()
    }
  }

  async start() {
    if (this.state === "starting" || this.state === "live") return
    if (!this.#supported) return

    this.state = "starting"
    this.submitted = false
    this.transcript = new Transcript()
    this.transcriptTarget.replaceChildren()
    this.resultTarget.hidden = true
    this.resultTarget.replaceChildren()
    this.#setStatus(MESSAGES.starting)
    this.#render()

    try {
      await this.#startAudio()
      this.session = this.#buildSession()
      await this.session.start()
      if (this.state !== "starting") return this.session?.close()

      this.state = "live"
      this.#setStatus(MESSAGES.live)
      this.#render()
      this.#acquireWakeLock()
    } catch (error) {
      if (this.state === "starting") {
        this.#teardown()
        this.#fail(this.#messageFor(error))
      }
    }
  }

  // Reconnects after an unexpected close, resuming the conversation when possible.
  async retry() {
    if (!this.session) return this.start()

    this.state = "starting"
    this.#setStatus(MESSAGES.reconnecting)
    this.#render()

    try {
      if (!this.audioContext || this.audioContext.state === "closed") await this.#startAudio()
      await withTimeout(this.audioContext.resume().catch(() => {}), 3000, "audio").catch(() => {})
      const { resumed } = await this.session.restart()
      if (this.state !== "starting") return

      this.state = "live"
      this.#setStatus(resumed ? MESSAGES.live : MESSAGES.resumeFailed)
      this.#render()
      this.#acquireWakeLock()
    } catch (error) {
      if (this.state === "starting") {
        this.#teardown()
        this.#fail(this.#messageFor(error))
      }
    }
  }

  stop() {
    this.#teardown()
    this.state = this.submitted ? "done" : "idle"
    if (!this.submitted) this.#setStatus(MESSAGES.stopped)
    this.#render()
  }

  // Session wiring

  #buildSession() {
    return new LiveSession({
      fetchToken: () => this.#fetchToken(),
      handlers: {
        onAudio: data => this.player?.enqueue(data),
        onInterrupted: () => this.player?.flush(),
        onTranscript: (role, text) => this.#appendTranscript(role, text),
        onTurnComplete: () => {
          this.transcript.endTurn()
          if (this.state === "finishing") this.#finishSoon()
        },
        onToolCall: call => this.#handleToolCall(call),
        onReconnecting: () => { if (this.state === "live") this.#setStatus(MESSAGES.reconnecting) },
        onReconnected: ({ resumed }) => {
          if (!this.submitted && !this.transcript.empty) this.session?.sendText(recapText(this.transcript))
          if (this.state === "live") this.#setStatus(resumed ? MESSAGES.live : MESSAGES.resumeFailed)
        },
        onClosed: ({ code, reason }) => this.#connectionLost(code, reason)
      }
    })
  }

  async #handleToolCall({ name, args }) {
    if (name !== "submit_incident") return { error: `Outil inconnu : ${name}` }
    if (this.submitted) return { result: "already_submitted" }

    this.#setStatus(MESSAGES.submitting)

    try {
      const report = await this.#postReport(args)
      this.submitted = true
      this.state = "finishing"
      this.#showResult(report)
      this.#setStatus(MESSAGES.submitted)
      this.#render()
      this.finishTimer = setTimeout(() => this.#finishSoon(), FINISH_TIMEOUT_MS)
      return { result: "ok" }
    } catch (error) {
      this.#setStatus(`${MESSAGES.submitError} ${error.message}`.trim())
      return { result: "error", error: "Le compte rendu n'a pas pu être publié. Préviens l'employé et propose de réessayer." }
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
    this.state = "done"
    this.#render()
  }

  #connectionLost(code, reason) {
    if (this.state === "finishing") return this.#finish()
    if (this.state !== "live") return

    console.warn("voice: connection closed", code, reason)
    this.player?.flush()
    this.#releaseWakeLock()
    this.state = "closed"
    this.#setStatus(`${MESSAGES.closed} Appuyez sur « Reprendre » pour continuer la conversation.`)
    this.#render()
  }

  #micLost() {
    if (this.state === "finishing") return this.#finish()
    if (this.state !== "live" && this.state !== "closed") return

    this.#teardown()
    this.#fail("Le micro a été coupé (appel entrant, autre application ou écran verrouillé). Appuyez sur « Réessayer ».")
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
    if (!response.ok) throw new Error(`(HTTP ${response.status})`)
    return response.json().catch(() => ({}))
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
      if (this.state === "live" || this.state === "finishing") this.session?.sendAudio(base64FromBytes(data))
    }
    this.source.connect(this.worklet)
    this.worklet.connect(this.audioContext.destination) // outputs silence; keeps the node pulled everywhere
    this.player = new Player(this.audioContext)

    this.stream.getAudioTracks().forEach(track => {
      track.onended = () => this.#micLost()
    })
  }

  #teardown() {
    clearTimeout(this.finishTimer)
    this.session?.close()
    this.session = null
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

  // Page

  #appendTranscript(role, text) {
    const line = this.transcript.append(role, text)
    if (!line) return

    if (line !== this.lastLine) {
      this.lastLine = line
      this.lastLineElement = document.createElement("p")
      this.lastLineElement.className = `voice__line voice__line--${role}`
      const label = document.createElement("strong")
      label.textContent = `${LABELS[role]} : `
      this.lastLineText = document.createTextNode("")
      this.lastLineElement.append(label, this.lastLineText)
      this.transcriptTarget.append(this.lastLineElement)
    }
    this.lastLineText.data = line.text
    this.lastLineElement.scrollIntoView?.({ block: "end", behavior: "smooth" })
  }

  #showResult({ message_url }) {
    const paragraph = document.createElement("p")
    paragraph.textContent = this.hasRoomNameValue && this.roomNameValue
      ? `Compte rendu publié dans « ${this.roomNameValue} ». `
      : "Compte rendu publié. "

    if (message_url) paragraph.append(this.#link(message_url, "Voir le message"))
    if (this.hasRoomUrlValue && this.roomUrlValue) {
      paragraph.append(" · ", this.#link(this.roomUrlValue, "Retour au salon"))
    }

    this.resultTarget.replaceChildren(paragraph)
    this.resultTarget.hidden = false
  }

  #link(href, text) {
    const link = document.createElement("a")
    link.href = href
    link.textContent = text
    return link
  }

  #setStatus(text) {
    if (this.hasStatusTarget) this.statusTarget.textContent = text
  }

  #fail(message, { retry = true } = {}) {
    this.state = retry ? "error" : "unavailable"
    this.#setStatus(message)
    this.#render()
  }

  #render() {
    if (!this.hasToggleTarget) return
    const button = this.toggleTarget
    const labels = {
      idle: "Démarrer", starting: "Annuler", live: "Terminer", finishing: "Terminer",
      closed: "Reprendre", error: "Réessayer", done: "Nouveau compte rendu", unavailable: "Indisponible"
    }
    button.textContent = labels[this.state] || "Démarrer"
    button.disabled = this.state === "unavailable"
    button.setAttribute("aria-pressed", String(this.state === "live" || this.state === "finishing"))
    this.element.dataset.voiceState = this.state
  }

  #messageFor(error) {
    if (error?.micError) {
      switch (error.name) {
        case "NotAllowedError":
        case "SecurityError":   return MESSAGES.micDenied
        case "NotFoundError":
        case "OverconstrainedError": return MESSAGES.micMissing
        default:                return `${MESSAGES.micError} (${error.name || "erreur"})`
      }
    }
    if (error instanceof StepTimeout) return `Bloqué à l'étape « ${STEP_LABELS[error.step] || error.step} ». Réessayez ; si cela se reproduit, signalez cette étape.`
    if (error instanceof TokenError) return error.status === 429 ? MESSAGES.rateLimited : `${MESSAGES.tokenError} (HTTP ${error.status})`
    if (error instanceof ConnectError) return `${MESSAGES.connectError} (code ${error.code}${error.reason ? `, ${error.reason}` : ""})`
    return `${MESSAGES.connectError} (${error?.message || error})`
  }

  get #supported() {
    return window.isSecureContext && navigator.mediaDevices?.getUserMedia && window.AudioWorkletNode
  }
}
