import { Controller } from "@hotwired/stimulus"

// Hermes fork: record a voice note from the composer and send it as an ordinary attachment.
//
// Tap the mic to start, tap again (it has become a send arrow) to stop and send; the bin discards
// and gives the focus back to the mic. While recording, a timer and a level meter run, and
// `warnSeconds` before `maxSeconds` a warning says the note will send itself; at `maxSeconds`
// recording stops and sends. The recorded File goes through the composer's own attachment
// path: it's handed over with the `drop-target:drop` event the composer already listens for on
// window (`drop-target:drop@window->composer#dropFiles`, what a file dropped on the room does),
// then the composer's send button is clicked. So the pending-upload bubble, the upload progress,
// the client_message_id and the Turbo Stream answer are exactly those of a picked file. Like the
// send button, that also sends whatever is typed in the composer and any other pending files.

const MAX_SECONDS = 300
const WARN_SECONDS = 30
const MIN_SECONDS = 1
const ERROR_MS = 6000

export const MESSAGES = {
  record: "Record a voice message",
  stop: "Send the voice message",
  cancel: "Discard the recording",
  insecure: "The microphone only works over HTTPS: open Meshduty at an https:// address.",
  unsupported: "This browser can't record a voice message.",
  micDenied: "Microphone access is blocked. Allow the microphone for this site in the browser's settings, then try again.",
  micMissing: "No microphone found on this device.",
  micError: "Couldn't open the microphone.",
  recordError: "The recording failed.",
  tooShort: "Record at least one second.",
  empty: "No sound was recorded.",
  offline: "Offline: the voice message will go when you tap Send.",
  autoSend: (seconds) => `Sending automatically in ${seconds}\u00a0s`
}

// MediaRecorder types by preference: [what to ask the recorder for, the File's type, extension].
// Safari only records audio/mp4.
export const FORMATS = [
  [ "audio/webm;codecs=opus", "audio/webm", "webm" ],
  [ "audio/mp4", "audio/mp4", "m4a" ],
  [ "audio/ogg;codecs=opus", "audio/ogg", "ogg" ]
]

// ---------------------------------------------------------------------------------------------
// Pure helpers

// The first supported format, or null (the recorder's default then).
export function pickFormat(isTypeSupported) {
  for (const [ recorderType, type, extension ] of FORMATS) {
    try {
      if (isTypeSupported(recorderType)) return { recorderType, type, extension }
    } catch {
      // Some browsers throw instead of answering false.
    }
  }
  return null
}

// The File's plain type and extension for what the recorder actually produced
// (`recorder.mimeType`, possibly with codecs, possibly empty).
export function fileFormat(mimeType, fallback) {
  const base = (mimeType || "").split(";")[0].trim().toLowerCase()
  if (base === "audio/webm" || base === "video/webm") return { type: "audio/webm", extension: "webm" }
  if (base === "audio/mp4" || base === "video/mp4" || base === "audio/x-m4a") return { type: "audio/mp4", extension: "m4a" }
  if (base === "audio/ogg" || base === "application/ogg") return { type: "audio/ogg", extension: "ogg" }
  if (fallback) return { type: fallback.type, extension: fallback.extension }
  return { type: "audio/webm", extension: "webm" }
}

const pad = (n) => String(n).padStart(2, "0")

// note-vocale-YYYYMMDD-HHMMSS.<extension>, in local time.
export function noteFilename(date, extension) {
  const day = `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}`
  const time = `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
  return `note-vocale-${day}-${time}.${extension}`
}

// 7 → "0:07", 125 → "2:05".
export function formatDuration(seconds) {
  const whole = Math.max(0, Math.floor(seconds))
  return `${Math.floor(whole / 60)}:${pad(whole % 60)}`
}

// Root mean square of time-domain samples (0 silence … 1 full scale), boosted so that speech
// fills most of the meter.
export function levelFromSamples(samples) {
  if (!samples || samples.length === 0) return 0
  let sum = 0
  for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i]
  return Math.min(1, Math.sqrt(sum / samples.length) * 4)
}

export function errorMessageFor(error) {
  switch (error?.name) {
    case "NotAllowedError":
    case "PermissionDeniedError":
    case "SecurityError":
      return MESSAGES.micDenied
    case "NotFoundError":
    case "DevicesNotFoundError":
    case "OverconstrainedError":
      return MESSAGES.micMissing
    default:
      return MESSAGES.micError
  }
}

const canCapture = () => !!(navigator.mediaDevices?.getUserMedia && window.MediaRecorder)

// ---------------------------------------------------------------------------------------------

export default class extends Controller {
  static targets = [ "toggle", "label", "timer", "cancel", "error", "level", "warning" ]
  static values = { maxSeconds: { type: Number, default: MAX_SECONDS }, warnSeconds: { type: Number, default: WARN_SECONDS } }

  #state = "idle" // idle | starting | recording | stopping
  #stream = null
  #recorder = null
  #chunks = []
  #format = null
  #startedAt = 0
  #elapsed = 0
  #ticker = null
  #errorTimer = null
  #send = false
  #refocus = false // give the focus back to the mic once idle again (after the bin)
  // Level meter (never `this.context`: that's Stimulus' own).
  #meterContext = null
  #analyser = null
  #meterFrame = null

  connect() {
    // Without HTTPS the browser hides getUserMedia: keep the button so a tap can say why.
    this.element.hidden = window.isSecureContext && !canCapture()
    this.#render()
  }

  disconnect() {
    this.#stop(false)
    this.#releaseMicrophone()
    clearTimeout(this.#errorTimer)
  }

  toggle() {
    if (this.#state === "recording") {
      this.#stop(true)
    } else if (this.#state === "idle") {
      this.#start()
    }
  }

  cancel() {
    if (this.#state === "starting") {
      this.#state = "idle"
      this.#releaseMicrophone()
      this.#render()
    } else {
      this.#stop(false)
    }
    this.#refocus = true
    this.#render()
  }

  async #start() {
    this.#hideError()

    if (!window.isSecureContext) return this.#showError(MESSAGES.insecure)
    if (!canCapture()) return this.#showError(MESSAGES.unsupported)

    this.#state = "starting"
    this.#render()

    let stream
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } })
    } catch (error) {
      if (this.#state === "starting") {
        this.#state = "idle"
        this.#render()
        this.#showError(errorMessageFor(error))
      }
      return
    }

    // Cancelled or disconnected while the permission prompt was up.
    if (this.#state !== "starting") {
      stream.getTracks().forEach((track) => track.stop())
      return
    }
    this.#stream = stream

    const format = pickFormat((type) => typeof MediaRecorder.isTypeSupported === "function" && MediaRecorder.isTypeSupported(type))
    let recorder
    try {
      recorder = format ? new MediaRecorder(stream, { mimeType: format.recorderType }) : new MediaRecorder(stream)
    } catch {
      this.#state = "idle"
      this.#releaseMicrophone()
      this.#render()
      return this.#showError(MESSAGES.unsupported)
    }

    const chunks = []
    recorder.addEventListener("dataavailable", (event) => {
      if (event.data && event.data.size > 0) chunks.push(event.data)
    })
    recorder.addEventListener("stop", () => this.#finished(recorder, chunks, format))
    recorder.addEventListener("error", () => {
      this.#send = false
      this.#showError(MESSAGES.recordError)
    })

    try {
      recorder.start()
    } catch {
      this.#state = "idle"
      this.#releaseMicrophone()
      this.#render()
      return this.#showError(MESSAGES.recordError)
    }

    this.#recorder = recorder
    this.#chunks = chunks
    this.#format = format
    this.#send = false
    this.#startedAt = performance.now()
    this.#elapsed = 0
    this.#state = "recording"
    this.#ticker = setInterval(() => this.#tick(), 250)
    this.#startMeter(stream)
    this.#render()
  }

  #startMeter(stream) {
    const AudioContextClass = window.AudioContext || window.webkitAudioContext
    if (!AudioContextClass || !this.hasLevelTarget) return
    try {
      this.#meterContext = new AudioContextClass()
      this.#meterContext.resume?.().catch(() => {})
      this.#analyser = this.#meterContext.createAnalyser()
      this.#analyser.fftSize = 1024
      this.#meterContext.createMediaStreamSource(stream).connect(this.#analyser)
    } catch {
      this.#stopMeter()
      return
    }

    const samples = new Float32Array(this.#analyser.fftSize)
    let level = 0
    const draw = () => {
      if (!this.#analyser) return
      this.#analyser.getFloatTimeDomainData(samples)
      level = Math.max(levelFromSamples(samples), level * 0.85) // quick rise, slow fall
      this.levelTarget.style.setProperty("--hermes-level", level.toFixed(3))
      this.#meterFrame = requestAnimationFrame(draw)
    }
    draw()
  }

  #stopMeter() {
    if (this.#meterFrame) cancelAnimationFrame(this.#meterFrame)
    this.#meterFrame = null
    this.#analyser = null
    this.#meterContext?.close?.().catch(() => {})
    this.#meterContext = null
    if (this.hasLevelTarget) this.levelTarget.style.removeProperty("--hermes-level")
  }

  #tick() {
    this.#elapsed = (performance.now() - this.#startedAt) / 1000
    if (this.#elapsed >= this.maxSecondsValue) {
      this.#stop(true)
    } else {
      this.#renderTimer()
    }
  }

  #stop(send) {
    clearInterval(this.#ticker)
    this.#ticker = null

    if (this.#state === "starting") {
      // getUserMedia still pending: #start drops the stream when it arrives.
      this.#state = "idle"
      this.#render()
      return
    }
    if (this.#state !== "recording") return

    this.#elapsed = (performance.now() - this.#startedAt) / 1000
    this.#send = send
    this.#state = "stopping"
    const recorder = this.#recorder
    try {
      if (recorder && recorder.state !== "inactive") {
        recorder.stop() // → the final dataavailable, then stop → #finished
      } else {
        this.#finished(recorder, this.#chunks, this.#format)
      }
    } catch {
      this.#finished(recorder, this.#chunks, this.#format)
    }
    this.#releaseMicrophone()
    this.#render()
  }

  #finished(recorder, chunks, format) {
    if (recorder !== this.#recorder) return
    const send = this.#send && this.#state === "stopping"
    const elapsed = this.#elapsed

    clearInterval(this.#ticker)
    this.#ticker = null
    this.#recorder = null
    this.#chunks = []
    this.#send = false
    this.#state = "idle"
    this.#releaseMicrophone()
    this.#render()

    if (!send) return
    if (elapsed < MIN_SECONDS) return this.#showError(MESSAGES.tooShort)

    const { type, extension } = fileFormat(recorder?.mimeType || chunks[0]?.type, format)
    const blob = new Blob(chunks, { type })
    if (blob.size === 0) return this.#showError(MESSAGES.empty)

    this.#deliver(new File([ blob ], noteFilename(new Date(), extension), { type, lastModified: Date.now() }))
  }

  // The composer's own attachment path: add the file like a drop, then press Send.
  #deliver(file) {
    window.dispatchEvent(new CustomEvent("drop-target:drop", { detail: { files: [ file ] } }))

    const form = this.element.closest("form")
    const send = form?.querySelector('button[name="send"]')
    if (send && !send.disabled && !send.closest("fieldset:disabled")) {
      send.click()
    } else {
      this.#showError(MESSAGES.offline)
    }
  }

  #releaseMicrophone() {
    this.#stopMeter()
    this.#stream?.getTracks().forEach((track) => track.stop())
    this.#stream = null
  }

  #render() {
    const recording = this.#state === "recording"
    const busy = this.#state === "starting" || this.#state === "stopping"

    if (recording) {
      this.element.dataset.recording = ""
    } else {
      delete this.element.dataset.recording
    }
    if (busy) {
      this.element.setAttribute("aria-busy", "true")
    } else {
      this.element.removeAttribute("aria-busy")
    }

    if (this.hasToggleTarget) {
      const label = recording ? MESSAGES.stop : MESSAGES.record
      this.toggleTarget.title = label
      this.toggleTarget.disabled = this.#state === "stopping"
      if (this.hasLabelTarget) this.labelTarget.textContent = label
      if (this.#refocus && this.#state === "idle") {
        this.#refocus = false
        this.toggleTarget.focus()
      }
    }
    if (this.hasCancelTarget) this.cancelTarget.hidden = !(recording || this.#state === "starting")
    this.#renderTimer()
  }

  #renderTimer() {
    if (this.hasTimerTarget) this.timerTarget.textContent = formatDuration(this.#elapsed)
    if (this.hasWarningTarget) {
      const left = this.maxSecondsValue - this.#elapsed
      const warning = this.#state === "recording" && left <= this.warnSecondsValue
        ? MESSAGES.autoSend(this.warnSecondsValue)
        : ""
      if (this.warningTarget.textContent !== warning) this.warningTarget.textContent = warning
    }
  }

  #showError(message) {
    if (!this.hasErrorTarget) return
    clearTimeout(this.#errorTimer)
    this.errorTarget.textContent = message
    this.errorTarget.hidden = false
    this.#errorTimer = setTimeout(() => this.#hideError(), ERROR_MS)
  }

  #hideError() {
    if (!this.hasErrorTarget) return
    clearTimeout(this.#errorTimer)
    this.errorTarget.hidden = true
    this.errorTarget.textContent = ""
  }
}
