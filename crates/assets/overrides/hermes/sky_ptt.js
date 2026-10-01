// Hermes fork: Sky push-to-talk, batch 1a (docs/hermes-gemini-live.md, "Sky push-to-talk").
//
// Hold the floating button (`#sky-ptt [data-sky-ptt-button]`), speak, let go: the words go to Sky
// (Gemini Live, straight from the browser with a token the server locked), which answers out loud
// and in writing in a card above the button. Slide up onto "Release here to cancel" to drop it; a
// tap shows a tip; a minute is the longest hold. Keyboard: focus the button and hold Space (or
// Enter), or hold Ctrl+Shift+Space anywhere outside a text field.
//
// One instance per document: the tab bar partial loads this module on every page, the browser runs
// it once, and `#sky-ptt` is `data-turbo-permanent`, so Turbo visits keep the element, the session
// and a reply that is still playing. Per-page facts (screen, room, hidden) come from the page's own
// `<template data-sky-page>`, read on every `turbo:load`. Nothing lives in a Stimulus controller or
// in nodes Turbo replaces.
//
// A press: the microphone opens at once (its audio is buffered) and `POST /sky/context` checks the
// screen and returns the note; once the hold is longer than a tap (TIP_MS), the session is opened if
// it isn't warm (`POST /sky/token`). With both there: the note, `activityStart`, the buffered audio,
// then live audio. Release: `activityEnd`. A tap sends nothing and mints no token. The session stays open SKY_WARM_SECONDS after the last reply
// (follow-ups keep their context), and closes when the page is hidden or left.
//
// The logic that doesn't touch the page is in hermes/sky_ptt_logic.js (`globalThis.HermesSky`,
// tested with `node --test`); the protocol is lib/hermes/live_session.js, shared with the voice page.

import { LiveSession, Player, TokenError, StepTimeout, INPUT_RATE, STEP_TIMEOUT_MS, withTimeout, base64FromBytes } from "lib/hermes/live_session"

const logic = globalThis.HermesSky
const STORAGE_KEY = "meshduty:sky-ptt:last"

const sky = {
  state: "ready",
  root: null,
  page: {},
  // The Gemini session (warm between presses) and its token's receipt and usage.
  session: null,
  connecting: null,
  tokenId: null,
  usage: { heldMs: 0, replyMs: 0, turns: 0 },
  warmTimer: null,
  // Audio: one AudioContext and worklet for the page's life; a microphone stream per press.
  audioContext: null,
  workletReady: null,
  worklet: null,
  player: null,
  stream: null,
  source: null,
  // The current press.
  press: null,
  lastPress: null,
  // Replies shown, newest last.
  exchanges: [],
  // Reply audio and words to ignore until the turn completes (a cancelled or interrupted turn).
  dropping: false,
  turnDone: true,
  replyTimer: null
}

// --- Start ----------------------------------------------------------------------------------------

if (!logic) {
  console.warn("Sky push-to-talk: hermes/sky_ptt_logic.js didn't load; reload the page.")
} else {
  document.addEventListener("turbo:load", onPage)
  if (document.readyState !== "loading") onPage()
  else document.addEventListener("DOMContentLoaded", onPage, { once: true })

  document.addEventListener("pointerdown", onPointerDown)
  document.addEventListener("pointermove", onPointerMove)
  document.addEventListener("pointerup", onPointerUp)
  document.addEventListener("pointercancel", onPointerCancel)
  document.addEventListener("contextmenu", event => { if (event.target.closest?.("[data-sky-ptt-button]")) event.preventDefault() })
  document.addEventListener("keydown", onKeyDown)
  document.addEventListener("keyup", onKeyUp)
  document.addEventListener("click", onClick)
  document.addEventListener("visibilitychange", () => { if (document.visibilityState === "hidden") closeSession("hidden") })
  window.addEventListener("pagehide", () => closeSession("pagehide"))
  window.addEventListener("online", () => { if (sky.state === "offline") setState("reset") })
  window.addEventListener("offline", () => { if (!logic.holding(sky.state)) setState("offline") })
}

// Each page (a Turbo visit or a full load): find the element, read the page's hint.
function onPage() {
  const root = document.getElementById("sky-ptt")
  const template = document.querySelector("template[data-sky-page]")
  if (!root || !template) {
    // Signed out, or a page without the button: nothing to keep open.
    closeSession("no button")
    sky.root = null
    return
  }
  const first = sky.root !== root
  sky.root = root
  sky.page = { ...template.dataset }
  const hidden = logic.hiddenOn(sky.page)
  root.hidden = hidden
  if (hidden && logic.holding(sky.state)) cancelPress("hidden")
  if (first) {
    if (!window.isSecureContext) return fail({ message: logic.TEXTS.insecure, state: "unsupported" })
    if (!navigator.mediaDevices?.getUserMedia || !window.AudioWorkletNode || !window.WebSocket) {
      return fail({ message: logic.TEXTS.unsupported, state: "unsupported" })
    }
    restoreLastReply()
    render()
  }
}

function el(selector) {
  return sky.root?.querySelector(selector)
}

function button() {
  return el("[data-sky-ptt-button]")
}

// --- Gestures -------------------------------------------------------------------------------------

function onPointerDown(event) {
  const target = event.target.closest?.("[data-sky-ptt-button]")
  if (!target || !sky.root || sky.root.hidden) return
  if (event.button !== undefined && event.button !== 0) return
  event.preventDefault()
  if (holdingNow()) return // a second finger, or a key already held
  try { target.setPointerCapture(event.pointerId) } catch {}
  press({ pointerId: event.pointerId, y: event.clientY })
}

function onPointerMove(event) {
  const current = sky.press
  if (!current || current.pointerId !== event.pointerId || current.released) return
  const cancelling = logic.inCancelZone(event.clientY - current.y)
  if (cancelling !== current.cancelling) {
    current.cancelling = cancelling
    render()
  }
}

function onPointerUp(event) {
  if (sky.press && sky.press.pointerId === event.pointerId) release()
}

// The system took the pointer (a scroll, a call): drop the request.
function onPointerCancel(event) {
  if (sky.press && sky.press.pointerId === event.pointerId) cancelPress("pointercancel")
}

function onKeyDown(event) {
  if (event.repeat) return
  const onButton = event.target.closest?.("[data-sky-ptt-button]")
  const viaButton = onButton && (event.key === " " || event.key === "Enter")
  const viaShortcut = logic.isShortcut(event, logic.isTyping(event.target))
  if (!viaButton && !viaShortcut) return
  if (!sky.root || sky.root.hidden) return
  event.preventDefault()
  if (!holdingNow()) press({ key: viaButton ? event.key : "shortcut", y: 0 })
}

// A press is held right now (not released yet).
function holdingNow() {
  return Boolean(sky.press && !sky.press.released)
}

function onKeyUp(event) {
  const current = sky.press
  if (!current || !current.key || current.released) return
  const done = current.key === "shortcut"
    ? (event.code === "Space" || event.key === "Control" || event.key === "Shift")
    : event.key === current.key
  if (!done) return
  event.preventDefault()
  release()
}

function onClick(event) {
  if (event.target.closest?.("[data-sky-dismiss]")) {
    sky.exchanges = []
    try { sessionStorage.removeItem(STORAGE_KEY) } catch {}
    render()
  }
}

// --- A press --------------------------------------------------------------------------------------

function press({ pointerId = null, key = null, y = 0 }) {
  if (sky.state === "unsupported") return render()
  if (navigator.onLine === false) return fail({ message: logic.TEXTS.offlineHelp, state: "offline" })

  // Interrupting Sky: stop its reply, drop the rest of that turn.
  const interrupting = logic.answering(sky.state)
  if (interrupting) {
    sky.player?.flush()
    if (!sky.turnDone) sky.dropping = true
    if (sky.press) sky.press.outcome = "interrupted"
  }
  finishPress()
  sky.error = null
  sky.flash = null

  const now = performance.now()
  sky.press = {
    pointerId, key, y, interrupting,
    pressedAt: now, releasedAt: NaN, contextAt: NaN, connectedAt: NaN, firstTextAt: NaN, firstAudioAt: NaN,
    warm: Boolean(sky.session?.open),
    cancelling: false, released: false, begun: false, ended: false, sendWhenReady: false,
    note: null, chip: null, buffer: [], streamedMs: 0, replyMs: 0, replyChars: 0, totalTokens: NaN,
    you: "", reply: "", outcome: "no_reply", timers: []
  }
  const current = sky.press
  setState("press")
  clearTimeout(sky.warmTimer)

  startAudioInGesture()
  // A cold session is opened once the hold is longer than a tap: a tap mints no token.
  current.timers.push(setTimeout(() => {
    if (sky.press !== current || current.outcome === "tip") return
    ensureSession(current)
    maybeBegin(current)
  }, logic.TIP_MS))
  current.timers.push(setTimeout(() => { if (sky.press === current && !current.released) render() }, logic.WARN_HOLD_MS))
  current.timers.push(setTimeout(() => { if (sky.press === current && !current.released) release({ limit: true }) }, logic.MAX_HOLD_MS))

  openMicrophone(current)
  fetchContext(current)
  if (current.warm) current.connectedAt = current.pressedAt
  render()
}

function release({ limit = false } = {}) {
  const current = sky.press
  if (!current || current.released) return
  current.released = true
  current.releasedAt = performance.now()
  const heldMs = current.releasedAt - current.pressedAt
  const kind = logic.classifyRelease({ heldMs, cancelling: current.cancelling })
  if (kind === "cancel") return cancelPress("slide")
  stopMicrophone()
  setAudioSession("playback")
  if (kind === "tip") {
    current.outcome = "tip"
    clearPressTimers(current)
    setState("short")
    // A tap while Sky spoke just stops it; otherwise, how to use the button.
    flash(current.interrupting ? logic.TEXTS.stopped : logic.TEXTS.tip)
    scheduleWarmClose()
    return
  }
  current.sendWhenReady = true
  if (limit) flash(logic.TEXTS.holdLimit)
  setState("release")
  addExchange(current)
  if (current.begun) endTurn(current)
  else maybeBegin(current)
}

// Drops the press: nothing is sent, or, if audio already streamed, the turn is ended and Sky is
// told to ignore it (its reply is dropped).
function cancelPress(reason) {
  const current = sky.press
  if (!current) return
  current.released = true
  current.releasedAt ||= performance.now()
  current.outcome = "cancelled"
  clearPressTimers(current)
  stopMicrophone()
  setAudioSession("playback")
  current.buffer = []
  if (current.begun && !current.ended) {
    current.ended = true
    sky.session?.endTurn()
    sky.session?.sendContext(logic.CANCEL_NOTE)
    sky.dropping = true
  }
  setState("cancel")
  flash(logic.TEXTS.cancelled)
  if (reason !== "slide") console.info("sky: press cancelled", reason)
  scheduleWarmClose()
}

// The turn starts once the hold passed TIP_MS, the note is back (or failed) and the session is
// open; a press released before that is sent then.
function maybeBegin(current) {
  if (sky.press !== current || current.begun || current.outcome === "cancelled" || current.outcome === "tip") return
  const held = (current.released ? current.releasedAt : performance.now()) - current.pressedAt
  if (held < logic.TIP_MS) return
  if (current.note === null || !sky.session?.open) return
  current.begun = true
  sky.dropping = false
  sky.turnDone = false
  if (!sky.session.beginTurn(current.note)) return failPress(current, { message: logic.TEXTS.connectionLost })
  for (const chunk of current.buffer) sky.session.sendAudio(chunk)
  current.buffer = []
  if (!current.released) setState("connected")
  if (current.sendWhenReady) endTurn(current)
  render()
}

function endTurn(current) {
  if (current.ended) return
  current.ended = true
  sky.session?.endTurn()
  sky.usage.heldMs += current.streamedMs
  sky.usage.turns += 1
  clearTimeout(sky.replyTimer)
  sky.replyTimer = setTimeout(() => {
    if (sky.press === current && logic.answering(sky.state) && !current.firstAudioAt && !current.firstTextAt) {
      failPress(current, { message: logic.TEXTS.noReply })
    }
  }, logic.REPLY_TIMEOUT_MS)
}

function failPress(current, { message, details = "", state = "fail" }) {
  if (current) {
    current.outcome = "error"
    clearPressTimers(current)
  }
  stopMicrophone()
  fail({ message, details, state })
}

function clearPressTimers(current) {
  for (const timer of current.timers) clearTimeout(timer)
  current.timers = []
}

// The previous press is over: its timings go with the next press.
function finishPress() {
  const previous = sky.press
  if (!previous) return
  clearPressTimers(previous)
  sky.lastPress = previous
  sky.press = null
}

// --- Context and session --------------------------------------------------------------------------

async function fetchContext(current) {
  const sheet = document.querySelector("dialog.ws-overlay[open] .ws-sheet[data-ws-sheet]")
  const hint = logic.pageHint(sky.page, sheet?.dataset.wsSheet ?? null)
  const last = logic.timingsReport(sky.lastPress)
  sky.lastPress = null
  try {
    const { response, body } = await postJson(sky.root.dataset.skyContextUrl, { ...hint, last })
    if (sky.press !== current) return
    current.contextAt = performance.now()
    if (!response.ok || response.redirected) {
      return failPress(current, { message: logic.replyError(response, body), details: `POST /sky/context: HTTP ${response.status}` })
    }
    current.note = typeof body.note === "string" ? body.note : ""
    current.chip = typeof body.chip === "string" ? body.chip : null
    render()
    maybeBegin(current)
  } catch (error) {
    if (sky.press !== current) return
    failPress(current, { message: logic.TEXTS.offlineHelp, details: String(error?.message || error), state: "offline" })
  }
}

function ensureSession(current) {
  if (sky.session?.open) {
    current.connectedAt = current.pressedAt
    return
  }
  if (!sky.connecting) sky.connecting = connect()
  sky.connecting.then(() => {
    if (sky.press !== current) return
    current.connectedAt = performance.now()
    maybeBegin(current)
  }, error => {
    if (sky.press !== current) return
    failPress(current, describeConnectError(error))
  })
}

async function connect() {
  const session = new LiveSession({
    fetchToken: options => fetchToken(options),
    handlers: {
      onAudio: onReplyAudio,
      onInterrupted: () => sky.player?.flush(),
      onTranscript: onTranscript,
      onTurnComplete: onTurnComplete,
      onUsage: usage => { if (sky.press && Number.isFinite(usage?.totalTokenCount)) sky.press.totalTokens = usage.totalTokenCount },
      onClosed: ({ code, reason }) => {
        console.warn("sky: connection closed", code, reason)
        if (sky.session === session) dropSession()
        if (logic.answering(sky.state) || logic.holding(sky.state)) failPress(sky.press, { message: logic.TEXTS.connectionLost, details: `WebSocket closed (code ${code})` })
      }
    }
  })
  try {
    await session.start()
    sky.session = session
    scheduleWarmClose()
  } finally {
    sky.connecting = null
  }
}

async function fetchToken({ resume } = {}) {
  const previous = sky.tokenId
  let response, body
  try {
    ({ response, body } = await postJson(sky.root.dataset.skyTokenUrl, resume && previous ? { reconnect_of: previous } : {}))
  } catch {
    throw new TokenError(0)
  }
  if (!response.ok || response.redirected || !body?.token || !body?.ws_url) {
    const error = new TokenError(response.redirected ? 401 : response.status)
    error.sentence = logic.replyError(response, body)
    throw error
  }
  // A new token: the previous one's session usage is reported (once), this one's starts.
  if (previous && previous !== body.token_id) reportUsage()
  sky.tokenId = body.token_id || null
  sky.warmSeconds = Number(body.warm_seconds) || Number(sky.root.dataset.skyWarmSeconds) || 120
  return body
}

function describeConnectError(error) {
  if (error instanceof TokenError) {
    return { message: error.sentence || logic.TEXTS.unavailable, details: `POST /sky/token: HTTP ${error.status}`, state: error.status === 0 ? "offline" : "fail" }
  }
  if (error instanceof StepTimeout) return { message: logic.TEXTS.connectError, details: `timeout: ${error.step}` }
  return { message: logic.TEXTS.connectError, details: String(error?.message || error) }
}

function scheduleWarmClose() {
  clearTimeout(sky.warmTimer)
  if (!sky.session) return
  const seconds = sky.warmSeconds || Number(sky.root?.dataset.skyWarmSeconds) || 120
  sky.warmTimer = setTimeout(() => {
    if (!logic.holding(sky.state) && !logic.answering(sky.state)) closeSession("idle")
  }, seconds * 1000)
}

function closeSession(reason) {
  clearTimeout(sky.warmTimer)
  if (sky.press && logic.holding(sky.state)) cancelPress(reason)
  if (logic.answering(sky.state)) {
    sky.player?.flush()
    if (sky.press) sky.press.outcome = "interrupted"
    setState("reset")
  }
  if (sky.session) {
    sky.session.close()
    dropSession()
  }
}

function dropSession() {
  reportUsage()
  sky.session = null
  sky.tokenId = null
}

// The session's usage, once per token (the server refuses a second report).
function reportUsage() {
  const tokenId = sky.tokenId
  if (!tokenId || !sky.root) return
  const report = logic.usageReport(tokenId, sky.usage)
  sky.usage = { heldMs: 0, replyMs: 0, turns: 0 }
  sky.tokenId = null
  postJson(sky.root.dataset.skyUsageUrl, report, { keepalive: true }).catch(() => {})
}

async function postJson(url, payload, { keepalive = false } = {}) {
  const response = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", "Accept": "application/json" },
    body: JSON.stringify(payload),
    keepalive
  })
  const body = await response.json().catch(() => ({}))
  return { response, body }
}

// --- Audio ----------------------------------------------------------------------------------------

// In the press's own event handler (iOS only starts audio from a gesture).
function startAudioInGesture() {
  try {
    sky.audioContext ||= new AudioContext()
    sky.audioContext.resume().catch(() => {})
  } catch (error) {
    console.warn("sky: no audio context", error)
  }
  setAudioSession("play-and-record")
}

// iOS 17+: the microphone while held, the loud speaker for the reply.
function setAudioSession(type) {
  try { if (navigator.audioSession) navigator.audioSession.type = type } catch {}
}

async function openMicrophone(current) {
  try {
    const stream = await withTimeout(navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }
    }), STEP_TIMEOUT_MS.mic, "mic")
    if (sky.press !== current || current.released) {
      stream.getTracks().forEach(track => track.stop())
      return
    }
    sky.stream = stream
    await ensureWorklet()
    if (sky.press !== current || current.released || sky.stream !== stream) return
    sky.source = sky.audioContext.createMediaStreamSource(stream)
    sky.source.connect(sky.worklet)
  } catch (error) {
    if (sky.press !== current) return
    if (error instanceof StepTimeout) return failPress(current, { message: logic.TEXTS.micError, details: "timeout: microphone permission" })
    const { message, help, state } = logic.micError(error)
    failPress(current, { message, details: help || `${error?.name || "Error"}: ${error?.message || ""}`, state: state === "no_permission" ? "denied" : "fail" })
  }
}

async function ensureWorklet() {
  const context = sky.audioContext
  if (!sky.workletReady) {
    sky.workletReady = withTimeout(context.audioWorklet.addModule(sky.root.dataset.skyWorkletUrl), STEP_TIMEOUT_MS.audio, "audio").then(() => {
      sky.worklet = new AudioWorkletNode(context, "pcm-capture", { numberOfOutputs: 1, processorOptions: { targetRate: INPUT_RATE, chunkMs: 100 } })
      sky.worklet.port.onmessage = ({ data }) => onMicChunk(data)
      sky.worklet.connect(context.destination) // outputs silence; keeps the node pulled everywhere
      sky.player = new Player(context, playing => onPlayback(playing))
    }).catch(error => {
      sky.workletReady = null
      throw error
    })
  }
  await sky.workletReady
}

function onMicChunk(data) {
  const current = sky.press
  if (!current || current.released || !sky.stream) return
  const base64 = base64FromBytes(data)
  current.streamedMs += (data.byteLength / 2) / INPUT_RATE * 1000
  if (current.begun) sky.session?.sendAudio(base64)
  else current.buffer.push(base64)
}

function stopMicrophone() {
  try { sky.source?.disconnect() } catch {}
  sky.source = null
  sky.stream?.getTracks().forEach(track => track.stop())
  sky.stream = null
}

// --- The reply ------------------------------------------------------------------------------------

function onReplyAudio(data) {
  if (sky.dropping) return
  const current = sky.press
  const ms = logic.audioMsFromBase64(data)
  sky.usage.replyMs += ms
  if (current) {
    current.replyMs += ms
    if (!current.firstAudioAt) current.firstAudioAt = performance.now()
  }
  sky.player?.enqueue(data)
  if (sky.state === "thinking") setState("reply")
}

function onTranscript(role, text) {
  if (sky.dropping) return
  const current = sky.press
  if (!current) return
  if (role === "user") {
    current.you += text
  } else {
    current.reply += text
    current.replyChars = current.reply.length
    if (!current.firstTextAt) current.firstTextAt = performance.now()
    if (sky.state === "thinking") setState("reply")
  }
  renderExchanges()
}

function onTurnComplete() {
  if (sky.dropping) {
    sky.dropping = false
    sky.turnDone = true
    return
  }
  sky.turnDone = true
  clearTimeout(sky.replyTimer)
  const current = sky.press
  if (current && current.begun) {
    current.outcome = current.reply || current.firstAudioAt ? "answered" : "no_reply"
    current.done = true
    saveLastReply(current)
    if (current.reply) announce(logic.displayText(current.reply))
  }
  if (!sky.player || sky.player.idle) settle()
  scheduleWarmClose()
}

function onPlayback(playing) {
  if (!playing && sky.turnDone) settle()
}

// The reply is over (all said and played).
function settle() {
  if (!logic.answering(sky.state)) return
  if (sky.press && !sky.press.done && sky.press.outcome !== "interrupted") return
  setState("done")
  finishPress()
  render()
}

// --- Rendering ------------------------------------------------------------------------------------

function setState(event) {
  const previous = sky.state
  sky.state = event === "press" && sky.state === "unsupported" ? sky.state : logic.next(sky.state, event)
  if (sky.state !== previous) {
    if (sky.state === "listening" || (sky.state === "warming" && previous !== "warming")) announce(logic.TEXTS.announceListening)
    if (sky.state === "thinking") announce(logic.TEXTS.announceThinking)
    if (sky.state === "cancelled") announce(logic.TEXTS.cancelled)
  }
  render()
}

function fail({ message, details = "", state = "fail" }) {
  sky.player?.flush()
  sky.error = { message, details }
  sky.state = logic.next(sky.state === "unsupported" ? "ready" : sky.state, state)
  if (state === "unsupported") sky.state = "unsupported"
  finishPress()
  announce(message)
  render()
}

function announce(text) {
  const status = el("[data-sky-status]")
  if (!status) return
  status.textContent = ""
  setTimeout(() => { status.textContent = text }, 50)
}

let flashTimer = null
function flash(text) {
  sky.flash = text
  clearTimeout(flashTimer)
  flashTimer = setTimeout(() => {
    sky.flash = null
    if ([ "tip", "cancelled" ].includes(sky.state)) sky.state = "ready"
    render()
  }, 2200)
  render()
}

function render() {
  const root = sky.root
  const target = button()
  if (!root || !target) return
  const current = sky.press
  const state = sky.state
  const cancelling = Boolean(current && !current.released && current.cancelling)
  target.dataset.state = cancelling ? "cancel" : state
  target.setAttribute("aria-pressed", String(logic.holding(state)))
  root.dataset.state = state

  const label = el("[data-sky-label]")
  if (label) {
    let text = logic.TEXTS[state] || logic.TEXTS.ready
    if (cancelling) text = logic.TEXTS.releaseToCancel
    else if (logic.holding(state) && current && logic.holdStage(performance.now() - current.pressedAt) === "warn") text = logic.TEXTS.holdWarning
    else if (state === "offline") text = logic.TEXTS.offline
    else if ([ "error", "no_permission", "unsupported", "tip", "cancelled" ].includes(state)) text = logic.TEXTS.ready
    label.textContent = text
  }
  const zone = el("[data-sky-cancel-zone]")
  if (zone) {
    zone.hidden = !(current && !current.released && current.pointerId !== null && logic.holding(state))
    zone.classList.toggle("sky-ptt__cancel--hot", cancelling)
  }
  renderExchanges()
}

function addExchange(current) {
  if (sky.exchanges.includes(current)) return
  sky.exchanges = logic.keepExchanges(sky.exchanges, current)
}

function renderExchanges() {
  const stack = el("[data-sky-stack]")
  if (!stack) return
  const current = sky.press
  const debug = sky.root.dataset.skyDebug === "true"
  const parts = []
  const shown = [ ...sky.exchanges ]
  if (current && logic.holding(sky.state) && !shown.includes(current)) shown.push(current)
  shown.forEach((exchange, index) => {
    const live = exchange === current && !exchange.done
    const header = live && logic.holding(sky.state) ? logic.TEXTS.listeningHeader
      : live && !exchange.reply ? logic.TEXTS.thinkingHeader : logic.TEXTS.replyHeader
    const card = document.createElement("section")
    card.className = "sky-ptt__card"
    card.innerHTML = `<p class="sky-ptt__who"><span class="sky-ptt__dot" aria-hidden="true">S</span><span data-header></span></p>`
    card.querySelector("[data-header]").textContent = header
    if (exchange.chip) appendText(card, "p", "sky-ptt__chip", `◎ ${exchange.chip}`)
    if (exchange.you || live) {
      const you = appendText(card, "p", "sky-ptt__you", logic.displayText(exchange.you))
      if (live && logic.holding(sky.state)) you.classList.add("sky-ptt__you--live")
    }
    if (exchange.reply) appendText(card, "p", "sky-ptt__reply", logic.displayText(exchange.reply))
    if (live && sky.state === "speaking") {
      const speaking = appendText(card, "p", "sky-ptt__speaking", "")
      speaking.innerHTML = `<span class="sky-ptt__wave" aria-hidden="true"><i></i><i></i><i></i><i></i><i></i></span>`
      speaking.append(logic.TEXTS.speaking)
    }
    if (debug && exchange.done && !exchange.restored) {
      appendText(card, "p", "sky-ptt__debug", logic.timingsText(logic.timingsReport(exchange)))
    }
    if (index === shown.length - 1 && !live) {
      const dismiss = document.createElement("button")
      dismiss.type = "button"
      dismiss.className = "sky-ptt__dismiss"
      dismiss.dataset.skyDismiss = ""
      dismiss.setAttribute("aria-label", logic.TEXTS.dismiss)
      dismiss.textContent = "✕"
      card.prepend(dismiss)
    }
    parts.push(card)
  })
  if (sky.error && [ "error", "offline", "no_permission", "unsupported" ].includes(sky.state)) {
    const notice = document.createElement("section")
    notice.className = "sky-ptt__card sky-ptt__card--notice"
    notice.setAttribute("role", "alert")
    appendText(notice, "p", "sky-ptt__reply", sky.error.message)
    if (sky.error.details) {
      const details = document.createElement("details")
      appendText(details, "summary", "", logic.TEXTS.details)
      appendText(details, "p", "sky-ptt__details", sky.error.details)
      notice.append(details)
    }
    parts.push(notice)
  } else if (sky.flash) {
    parts.push(Object.assign(document.createElement("p"), { className: "sky-ptt__hint", textContent: sky.flash }))
  }
  stack.replaceChildren(...parts)
  stack.hidden = parts.length === 0
  stack.scrollTop = stack.scrollHeight
}

function appendText(parent, tag, className, text) {
  const node = document.createElement(tag)
  if (className) node.className = className
  node.textContent = text
  parent.append(node)
  return node
}

// --- The last reply, across full page loads -------------------------------------------------------

function saveLastReply(exchange) {
  try {
    sessionStorage.setItem(STORAGE_KEY, JSON.stringify({ at: Date.now(), chip: exchange.chip, you: exchange.you, reply: exchange.reply }))
  } catch {}
}

function restoreLastReply() {
  try {
    const saved = JSON.parse(sessionStorage.getItem(STORAGE_KEY) || "null")
    if (!saved || typeof saved.reply !== "string" || Date.now() - saved.at > logic.RESTORE_MAX_AGE_MS) return
    sky.exchanges = [ { chip: saved.chip || null, you: String(saved.you || ""), reply: saved.reply, done: true, restored: true } ]
  } catch {}
}
