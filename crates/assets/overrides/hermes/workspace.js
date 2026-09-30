// Hermes fork: the Duty Manager Workspace's page behaviour (docs/hermes-workspace.md).
//
// Loaded as a module by the tab bar partial, which the layout renders only while the workspace is
// on, so it runs once per document; everything works by delegation and by watching the document,
// so it keeps working across Turbo navigations and for messages that arrive over Action Cable.
//
// - Card chips: links marked `data-ws-card` (chips, and card links the server didn't know yet) are
//   refreshed from GET /workspace/cards.json, so a chip shows the card's current state even inside
//   a cached message fragment.
// - Draft buttons (`.ws-draft`): File / Dismiss POST the decision; the server posts "confirm" or
//   "cancel" in the room as the user, mentioning the bot. Edit puts the mention and "change: " in the
//   composer. A draft followed by a later message from the same bot is marked answered.
// - Card sheet (phase 1): a chip, a card in a room's panel or on the board opens the card's sheet
//   in an overlay (GET /workspace/cards/:n?fragment=1); its controls POST JSON to
//   /workspace/cards/:n/<change> and the sheet, the chips and the panel are refreshed from the reply.
//   Home's tiles, its mentions and the Hermes log's cards open it too. "Show earlier comments"
//   reloads the sheet in place with ?comments=all (a plain link to that page without the script).
// - Board: one column at a time on phones (the column switcher), Move menus on cards.
// - Room panel: the cards of the departments linked to the room, behind a "N cards" button the
//   script adds to the room's nav; open or closed is remembered per browser.
// - "Create a card": an entry in every message's action menu (and New card buttons) opens the
//   new-card form (GET /workspace/cards/new?fragment=1), posted as JSON to /workspace/cards. Only
//   when the policy lets the viewer create cards (`[data-ws-viewer][data-ws-can-create]`).
// - A failed change in the sheet shows the error and keeps what was typed in the comment box.
// - Settings: the departments rows (add / remove) and the form, posted as JSON.
// - Visibility (phase 2.7): with restricted departments, message HTML only marks card links, and the
//   chips come from `cards.json`, which answers per viewer; a chip it leaves out becomes a plain link.
// - Handover (phase 2.6): the handover page's text is posted as JSON to /workspace/handover.
// - Hermes (phase 2): a Hermes proposal's draft (`[data-ws-proposal]`) has the same buttons, posting
//   to the proposal's decision route; its state (filed, dismissed, timed out) comes from
//   GET /workspace/hermes/proposals.json, since the buttons live in the cached message HTML. The
//   Hermes tab's Undo buttons POST /workspace/hermes/actions/:id/undo.
//
// The logic that doesn't touch the page is in hermes/workspace_logic.js (tested with `node --test`),
// which the tab bar loads first and which is read from `globalThis.HermesWorkspace`.
//
// Every write answers JSON; a refusal or an error shows its message where the action was. A reply
// that was redirected means the session expired (fetch follows the redirect to the sign-in page).

const logic = globalThis.HermesWorkspace
// workspace_logic.js didn't load (a failed request, a stale cached page): the page stays as the
// server rendered it (links work, the workspace's buttons say to reload) instead of throwing on
// every event.
const ready = Boolean(logic)
if (!ready) console.warn("Hermes workspace: hermes/workspace_logic.js didn't load; reload the page to use the workspace's buttons.")

// A document listener that runs only once the logic is there.
function onEvent(type, handler) {
  document.addEventListener(type, event => ready ? handler(event) : undefined)
}
const CARDS_URL = "/workspace/cards.json"
const PROPOSALS_URL = "/workspace/hermes/proposals.json"
const REFRESH_MS = 60_000

let scanTimer = null

function scheduleScan(delay = 250) {
  clearTimeout(scanTimer)
  scanTimer = setTimeout(scan, delay)
}

function scan() {
  if (!ready) return markUnavailable()
  markAnsweredDrafts()
  refreshProposals()
  refreshChips()
  addCardActions()
  setUpPanel()
}

function markUnavailable() {
  for (const draft of document.querySelectorAll(".ws-draft:not([data-ws-draft-state])")) {
    draft.dataset.wsDraftState = "sent"
    setStatus(draft, "Reload the page to use these buttons.")
  }
}

// --- Card chips ------------------------------------------------------------------------------------

async function refreshChips() {
  const links = [ ...document.querySelectorAll("a[data-ws-card]") ]
  const numbers = logic.chipNumbers(links.map(link => link.dataset.wsCard))
  if (numbers.length === 0) return

  const url = document.querySelector(".ws-tabbar")?.dataset.wsCardsUrl || CARDS_URL
  let reply, cards
  try {
    const response = await fetch(`${url}?numbers=${numbers.join(",")}`, { credentials: "same-origin", headers: { "Accept": "application/json" } })
    // Signed out: the session check redirects to the sign-in page, which fetch follows (200, HTML).
    if (!response.ok || response.redirected) return
    reply = await response.json()
    cards = reply?.cards || {}
  } catch {
    return // Fizzy or the network is down: links stay as they are
  }

  // Visibility restricted (phase 2.7): a chip of a card this viewer may not see (in a page rendered
  // before) goes back to being a plain link.
  const shown = [ ...document.querySelectorAll("a.ws-chip[data-ws-card]") ].map(chip => chip.dataset.wsCard)
  for (const number of logic.chipsToHide(shown, reply)) {
    for (const chip of document.querySelectorAll(`a.ws-chip[data-ws-card="${number}"]`)) {
      const link = document.createElement("a")
      link.href = chip.getAttribute("href") || "#"
      link.dataset.wsCard = number
      link.textContent = link.href
      chip.replaceWith(link)
    }
  }

  for (const link of document.querySelectorAll("a[data-ws-card]")) {
    const html = cards[link.dataset.wsCard]
    if (!html) continue
    const version = logic.hash(html)
    if (link.dataset.wsV === version) continue
    const template = document.createElement("template")
    template.innerHTML = html
    const chip = template.content.firstElementChild
    if (!chip) continue
    chip.dataset.wsV = version
    link.replaceWith(chip)
  }
}

// --- Drafts -----------------------------------------------------------------------------------------

// A text draft followed by a later message of the same bot was answered (or re-drafted). Not a
// proposal's draft: Hermes's own reply comes right after it, and the proposal says its state.
function markAnsweredDrafts() {
  for (const draft of document.querySelectorAll(".ws-draft:not([data-ws-draft-state]):not([data-ws-proposal])")) {
    const message = draft.closest(".message")
    if (!message) continue
    for (let next = message.nextElementSibling; next; next = next.nextElementSibling) {
      if (next.classList.contains("message") && next.dataset.userId === message.dataset.userId) {
        draft.dataset.wsDraftState = "superseded"
        break
      }
    }
  }
}

function setStatus(draft, text) {
  const status = draft.querySelector(".ws-draft__status")
  if (status) status.textContent = text
}

async function answerDraft(draft, decision) {
  const proposal = "wsProposal" in draft.dataset
  draft.dataset.wsDraftState = "busy"
  setStatus(draft, logic.draftBusyText(decision))
  try {
    const response = await fetch(draft.dataset.wsDraftUrl, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "Accept": "application/json" },
      body: JSON.stringify({ decision })
    })
    const body = await response.json().catch(() => ({}))
    // Signed out: nothing was posted.
    const problem = logic.replyProblem(response, body) || (response.status === 201 ? null : `HTTP ${response.status}`)
    if (problem) throw new Error(problem)
    draft.dataset.wsDraftState = "sent"
    setStatus(draft, logic.draftSentText(decision, body, proposal))
    if (body.chip && body.card) updateChips(body.card, body.chip)
    if (proposal) afterHermesChange()
  } catch (error) {
    delete draft.dataset.wsDraftState
    setStatus(draft, proposal ? error.message : `Couldn’t send: ${error.message}`)
  }
}

// --- Hermes: proposals' states, undo -----------------------------------------------------------------

async function refreshProposals() {
  const drafts = [ ...document.querySelectorAll(".ws-draft[data-ws-proposal]:not([data-ws-draft-state])") ]
  const ids = logic.proposalIds(drafts.map(draft => draft.dataset.wsProposal))
  if (ids.length === 0) return
  let states
  try {
    const response = await fetch(`${PROPOSALS_URL}?ids=${ids.join(",")}`, { credentials: "same-origin", headers: { "Accept": "application/json" } })
    if (!response.ok || response.redirected) return
    states = (await response.json())?.proposals || {}
  } catch {
    return // the buttons stay; the server refuses what isn't pending any more
  }
  for (const draft of drafts) {
    const text = logic.proposalStateText(states[draft.dataset.wsProposal])
    if (!text) continue
    draft.dataset.wsDraftState = "sent"
    setStatus(draft, text)
  }
}

// On the Hermes tab, show the log as it is now.
function afterHermesChange() {
  if (document.querySelector("[data-ws-hermes]")) setTimeout(reloadPage, 900)
}

onEvent("click", async event => {
  const button = event.target.closest?.("[data-ws-undo]")
  if (!button || button.disabled) return
  event.preventDefault()
  const item = button.closest(".ws-log__item")
  button.disabled = true
  showStatus(item, "Undoing…")
  try {
    const data = await postJSON(button.dataset.wsUndo, {})
    showStatus(item, data.message || "Undone.", "ok")
    afterHermesChange()
  } catch (error) {
    button.disabled = false
    showStatus(item, error.message, "error")
  }
})

function escapeHTML(text) {
  const element = document.createElement("span")
  element.textContent = text
  return element.innerHTML
}

function escapeAttribute(text) {
  return escapeHTML(text).replaceAll("\"", "&quot;")
}

// The composer's content: a mention of the bot (the attachment the mention prompt inserts, so the
// bot gets the message in a shared room), then "change: ".
function editDraft(draft) {
  const form = document.querySelector("form#composer")
  const editor = form?.querySelector("[data-composer-target='text']")
  if (!editor) return

  const name = draft.dataset.wsBotName || "Hermes"
  const mention = `<span class="mention">${escapeHTML(name)}</span>`
  const content = `<p><action-text-attachment sgid="${escapeAttribute(draft.dataset.wsBotSgid || "")}" content-type="application/vnd.campfire.mention" content="${escapeAttribute(mention)}"></action-text-attachment> change: </p>`

  const composer = window.Stimulus?.getControllerForElementAndIdentifier(form, "composer")
  if (composer?.replaceMessageContent) {
    composer.replaceMessageContent(content)
  } else {
    editor.value = content
    editor.focus()
  }
}

onEvent("click", event => {
  const button = event.target.closest?.("[data-ws-draft-action]")
  if (!button) return
  const draft = button?.closest(".ws-draft")
  if (!draft || draft.dataset.wsDraftState === "busy") return

  event.preventDefault()
  const action = button.dataset.wsDraftAction
  if (action === "edit") {
    editDraft(draft)
  } else {
    answerDraft(draft, action)
  }
})

// --- Requests ---------------------------------------------------------------------------------------

// POSTs `body` as JSON. Resolves to the reply's JSON; rejects with a message to show.
async function postJSON(url, body) {
  let response
  try {
    response = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "Accept": "application/json" },
      body: JSON.stringify(body)
    })
  } catch {
    throw new Error(logic.UNREACHABLE)
  }
  const data = await response.json().catch(() => ({}))
  const problem = logic.replyProblem(response, data)
  if (problem) throw new Error(problem)
  return data
}

// GETs an HTML fragment.
async function getFragment(url) {
  const separator = url.includes("?") ? "&" : "?"
  let response
  try {
    response = await fetch(`${url}${separator}fragment=1`, { credentials: "same-origin", headers: { "Accept": "text/html" } })
  } catch {
    throw new Error(logic.UNREACHABLE)
  }
  if (response.redirected) throw new Error(logic.SIGNED_OUT)
  if (response.status === 404) throw new Error("This card isn’t on the incident board.")
  const html = await response.text()
  // Errors come as a notice to show as is.
  return { ok: response.ok, status: response.status, html }
}

function fragment(html) {
  const template = document.createElement("template")
  template.innerHTML = html.trim()
  return template.content.firstElementChild
}

function showStatus(container, text, tone = "") {
  const status = container?.querySelector("[data-ws-status]")
  if (!status) return
  status.textContent = text
  status.dataset.tone = tone
}

// --- Overlay (card sheet, new-card form) -------------------------------------------------------------

let overlayDirty = false

function overlay() {
  let dialog = document.querySelector("dialog.ws-overlay")
  if (!dialog) {
    dialog = document.createElement("dialog")
    dialog.className = "ws-overlay"
    dialog.setAttribute("aria-label", "Card")
    dialog.addEventListener("close", onOverlayClosed)
    document.body.append(dialog)
  }
  return dialog
}

async function openOverlay(url) {
  const dialog = overlay()
  dialog.innerHTML = `<p class="ws-overlay__loading" role="status">Loading…</p>`
  if (!dialog.open) dialog.show()
  document.documentElement.classList.add("ws-overlay-open")
  try {
    const { html } = await getFragment(url)
    dialog.replaceChildren(fragment(html) || document.createTextNode(""))
    dialog.querySelector("input[name=title], textarea, select")?.focus({ preventScroll: true })
  } catch (error) {
    dialog.innerHTML = ""
    const notice = document.createElement("p")
    notice.className = "ws-notice"
    notice.textContent = error.message
    const close = document.createElement("button")
    close.type = "button"
    close.className = "btn ws-btn"
    close.dataset.wsSheetClose = ""
    close.textContent = "Close"
    dialog.append(notice, close)
  }
}

function closeOverlay() {
  const dialog = document.querySelector("dialog.ws-overlay")
  if (dialog?.open) dialog.close()
}

function onOverlayClosed() {
  document.documentElement.classList.remove("ws-overlay-open")
  // The board shows the cards as of the page load: reload it once something changed.
  if (overlayDirty && document.querySelector("[data-ws-board]")) reloadPage()
  overlayDirty = false
}

function reloadPage() {
  if (window.Turbo?.visit) {
    window.Turbo.visit(location.href, { action: "replace" })
  } else {
    location.reload()
  }
}

// A plain click (no modifier: those keep the link's own behaviour, e.g. the sheet's page in a new tab).
function plainClick(event) {
  return event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey
}

onEvent("click", event => {
  if (!plainClick(event)) return
  const earlier = event.target.closest?.(".ws-sheet a[data-ws-earlier-comments]")
  if (earlier) {
    event.preventDefault()
    showEarlierComments(earlier.closest(".ws-sheet"), earlier.getAttribute("href"))
    return
  }

  const opener = event.target.closest?.("a.ws-chip[data-ws-card], [data-ws-open-sheet]")
  if (opener) {
    const number = opener.dataset.wsOpenSheet || opener.dataset.wsCard
    if (!/^\d+$/.test(number)) return
    event.preventDefault()
    openOverlay(`/workspace/cards/${number}`)
    return
  }

  const newCard = event.target.closest?.("[data-ws-new-card]")
  if (newCard) {
    event.preventDefault()
    newCard.closest("details")?.removeAttribute("open")
    const query = new URLSearchParams()
    if (newCard.dataset.wsMessage) query.set("message_id", newCard.dataset.wsMessage)
    else if (newCard.dataset.wsRoom) query.set("room_id", newCard.dataset.wsRoom)
    openOverlay(`/workspace/cards/new?${query}`)
    return
  }

  if (event.target.closest?.("[data-ws-sheet-close]")) {
    event.preventDefault()
    if (event.target.closest("dialog.ws-overlay")) closeOverlay()
    else location.href = "/workspace/board"
  }
})

document.addEventListener("keydown", event => {
  if (event.key === "Escape" && document.querySelector("dialog.ws-overlay[open]")) closeOverlay()
})

document.addEventListener("turbo:before-cache", () => {
  closeOverlay()
  document.querySelector("dialog.ws-overlay")?.remove()
})

// --- Card sheet ------------------------------------------------------------------------------------

function changeBody(control) {
  return logic.changeBody(control.dataset.wsChange, {
    value: control.value,
    checked: control.checked,
    checkedValues: control.dataset.wsChange === "departments" ? [ ...control.querySelectorAll("input:checked") ].map(input => input.value) : [],
    stepId: control.dataset.wsStep
  })
}

async function changeCard(sheet, kind, body) {
  sheet.setAttribute("aria-busy", "true")
  showStatus(sheet, "Saving…")
  try {
    const allComments = sheet.dataset.wsComments === "all"
    const data = await postJSON(logic.sheetUrl(sheet.dataset.wsActionUrl, { change: kind, allComments }), body)
    overlayDirty = true
    if (data.chip) updateChips(data.number, data.chip)
    refreshPanel()
    const next = data.sheet && fragment(data.sheet)
    if (next) {
      // A posted comment clears the box; any other change keeps what was being typed.
      if (kind !== "comment") keepTyped(sheet, next)
      sheet.replaceWith(next)
      showStatus(next, "Saved.", "ok")
    } else {
      showStatus(sheet, "Saved.", "ok")
    }
  } catch (error) {
    showStatus(sheet, error.message, "error")
    // Show the card as it really is now (a toggle may have half-applied), keeping what was typed
    // in the comment box: a failed comment (or any failed change) mustn't lose it.
    if (sheet.closest("dialog.ws-overlay")) {
      const allComments = sheet.dataset.wsComments === "all"
      const { ok, html } = await getFragment(logic.sheetUrl(sheet.dataset.wsActionUrl, { allComments })).catch(() => ({ ok: false }))
      const next = ok && fragment(html)
      if (next) {
        keepTyped(sheet, next)
        sheet.replaceWith(next)
        showStatus(next, error.message, "error")
      }
    }
  } finally {
    sheet.removeAttribute("aria-busy")
  }
}

// The sheet again with every comment (up to the server's cap), keeping what was typed.
async function showEarlierComments(sheet, url) {
  if (sheet.getAttribute("aria-busy") === "true") return
  sheet.setAttribute("aria-busy", "true")
  showStatus(sheet, "Loading earlier comments…")
  try {
    const { ok, html } = await getFragment(url)
    const next = fragment(html)
    // Not ok: a notice (busy, 429; Fizzy down, 502) to show as the sheet's status.
    if (!ok || !next) throw new Error(next?.textContent?.trim() || "The earlier comments couldn’t be loaded; try again in a moment.")
    keepTyped(sheet, next)
    sheet.replaceWith(next)
  } catch (error) {
    showStatus(sheet, error.message, "error")
  } finally {
    sheet.removeAttribute("aria-busy")
  }
}

// What was typed in `sheet`'s text boxes (the comment box, any other), into the same boxes of
// `next`, the sheet that replaces it.
function keepTyped(sheet, next) {
  for (const field of sheet.querySelectorAll("textarea[name], input[name]:not([type=checkbox]):not([type=radio]):not([type=hidden])")) {
    if (!field.value) continue
    const same = next.querySelector(`${field.localName}[name="${CSS.escape(field.name)}"]`)
    if (same) same.value = field.value
  }
}

function updateChips(number, html) {
  for (const link of document.querySelectorAll(`a[data-ws-card="${number}"]`)) {
    const chip = fragment(html)
    if (!chip) continue
    chip.dataset.wsV = logic.hash(html)
    link.replaceWith(chip)
  }
}

onEvent("change", event => {
  const control = event.target.closest?.(".ws-sheet [data-ws-change]")
  const sheet = control?.closest(".ws-sheet")
  if (!sheet || sheet.getAttribute("aria-busy") === "true") return
  const body = changeBody(control)
  if (body) changeCard(sheet, control.dataset.wsChange, body)
})

onEvent("submit", event => {
  const form = event.target
  if (form.matches?.(".ws-sheet [data-ws-comment]")) {
    event.preventDefault()
    const sheet = form.closest(".ws-sheet")
    const body = form.elements.body.value.trim()
    if (body && sheet.getAttribute("aria-busy") !== "true") changeCard(sheet, "comment", { body })
  } else if (form.matches?.("[data-ws-new-card-form]")) {
    event.preventDefault()
    createCard(form)
  } else if (form.matches?.("[data-ws-settings-form]")) {
    event.preventDefault()
    saveSettings(form)
  } else if (form.matches?.("[data-ws-handover-form]")) {
    event.preventDefault()
    postHandover(form)
  }
})

// --- Board -----------------------------------------------------------------------------------------

onEvent("click", async event => {
  const tab = event.target.closest?.("[data-ws-coltab]")
  if (tab) {
    const board = tab.closest("[data-ws-board]")
    for (const other of board.querySelectorAll("[data-ws-coltab]")) other.setAttribute("aria-pressed", String(other === tab))
    for (const column of board.querySelectorAll("[data-ws-col]")) column.classList.toggle("ws-col--active", column.dataset.wsCol === tab.dataset.wsColtab)
    return
  }

  const move = event.target.closest?.("[data-ws-move]")
  if (move) {
    const board = move.closest("[data-ws-board]")
    move.closest("details")?.removeAttribute("open")
    showStatus(board, `Moving No. ${move.dataset.wsNumber}…`)
    try {
      await postJSON(`/workspace/cards/${move.dataset.wsNumber}/move`, { to: move.dataset.wsMove })
      reloadPage()
    } catch (error) {
      showStatus(board, error.message, "error")
    }
  }
})

// --- Room panel ------------------------------------------------------------------------------------

const PANEL_KEY = "ws-panel-open"

function panelWanted() {
  try { return localStorage.getItem(PANEL_KEY) === "1" } catch { return false }
}

function rememberPanel(open) {
  try { localStorage.setItem(PANEL_KEY, open ? "1" : "0") } catch {}
}

function setUpPanel() {
  const root = document.querySelector("[data-ws-panel-root]")
  const nav = document.querySelector("#nav")
  let toggle = nav?.querySelector(".ws-panel-toggle")
  if (!root || !nav) {
    toggle?.remove()
    return
  }
  if (!toggle) {
    toggle = document.createElement("button")
    toggle.type = "button"
    toggle.className = "btn ws-panel-toggle"
    toggle.setAttribute("aria-controls", "ws-panel")
    toggle.addEventListener("click", () => showPanel(document.querySelector("#ws-panel")?.hidden ?? false))
    const current = nav.querySelector(".room--current")
    current ? current.after(toggle) : nav.append(toggle)
  }
  const count = Number(root.dataset.wsPanelCount || 0)
  toggle.textContent = `${count} ${count === 1 ? "card" : "cards"}`
  if (root.dataset.wsPanelReady) return
  root.dataset.wsPanelReady = "1"
  // Wide screens keep it open if it was; phones open it on demand (it covers the room).
  showPanel(panelWanted() && matchMedia("(min-width: 100ch)").matches, false)
}

function showPanel(open, remember = true) {
  const panel = document.querySelector("#ws-panel")
  if (!panel) return
  panel.hidden = !open
  document.body.classList.toggle("ws-panel-open", open)
  document.querySelector(".ws-panel-toggle")?.setAttribute("aria-expanded", String(open))
  if (remember) rememberPanel(open)
}

async function refreshPanel() {
  const root = document.querySelector("[data-ws-panel-root]")
  if (!root) return
  const open = !document.querySelector("#ws-panel")?.hidden
  try {
    const response = await fetch(root.dataset.wsPanelUrl, { credentials: "same-origin", headers: { "Accept": "text/html" } })
    if (!response.ok || response.redirected || response.status === 204) return
    const next = fragment(await response.text())
    if (!next) return
    next.dataset.wsPanelReady = "1"
    root.replaceWith(next)
    showPanel(open, false)
    setUpPanel()
  } catch {
    // Kept as it is; the next page load shows it again.
  }
}

onEvent("click", event => {
  if (event.target.closest?.("[data-ws-panel-close]")) showPanel(false)
})

// --- "Create a card from this message" -------------------------------------------------------------

// The policy lets the viewer create cards (the layout's overlay says; the server checks again).
function canCreateCards() {
  return document.querySelector("[data-ws-viewer]")?.dataset.wsCanCreate !== "false"
}

function addCardActions() {
  if (!document.querySelector("meta[name='current-room-id']")) return
  if (!canCreateCards()) {
    for (const button of document.querySelectorAll(".ws-card-action")) button.remove()
    return
  }
  const icon = document.querySelector(".ws-tabbar a[href='/workspace/board'] img")?.getAttribute("src")
  for (const grid of document.querySelectorAll(".message .message__actions-grid:not([data-ws-card-action])")) {
    const message = grid.closest(".message")
    if (!message?.dataset.messageId) continue
    grid.dataset.wsCardAction = ""
    const button = document.createElement("button")
    button.type = "button"
    button.className = "btn message__action-btn center full-width ws-card-action"
    button.title = "Create a card from this message"
    button.setAttribute("aria-label", "Create a card from this message")
    button.dataset.wsNewCard = ""
    button.dataset.wsMessage = message.dataset.messageId
    if (icon) {
      const image = document.createElement("img")
      image.className = "colorize--black"
      image.src = icon
      image.width = image.height = 20
      image.setAttribute("aria-hidden", "true")
      button.append(image)
    } else {
      button.textContent = "Card"
    }
    grid.append(button)
  }
}

async function createCard(form) {
  const submit = form.querySelector("[type=submit]")
  const value = name => form.elements[name]?.value ?? ""
  const body = {
    title: value("title"),
    description: value("description"),
    severity: value("severity"),
    department: value("department"),
    message_id: value("message_id"),
    room_id: value("room_id")
  }
  submit.disabled = true
  showStatus(form, "Creating the card…")
  try {
    const data = await postJSON(form.action, body)
    overlayDirty = true
    refreshPanel()
    showStatus(form, data.warning || `Card #${data.number} created.`, data.warning ? "error" : "ok")
    if (data.warning) {
      submit.remove()
    } else {
      setTimeout(closeOverlay, 900)
    }
  } catch (error) {
    submit.disabled = false
    showStatus(form, error.message, "error")
  }
}

// --- Settings --------------------------------------------------------------------------------------

onEvent("click", event => {
  if (event.target.closest?.("[data-ws-add-department]")) {
    const form = event.target.closest("form")
    const template = form.querySelector("template[data-ws-department-template]")
    const list = form.querySelector("[data-ws-departments]")
    const row = template.content.firstElementChild.cloneNode(true)
    list.append(row)
    row.querySelector("input[name=name]")?.focus()
  } else if (event.target.closest?.("[data-ws-remove-department]")) {
    event.target.closest("[data-ws-department]")?.remove()
  }
})

async function saveSettings(form) {
  const departments = [ ...form.querySelectorAll("[data-ws-departments] [data-ws-department]") ].map(row => ({
    name: row.querySelector("input[name=name]").value,
    tag: row.querySelector("input[name=tag]").value,
    rooms: [ ...row.querySelectorAll("input[name=rooms]:checked") ].map(input => input.value),
    restricted: Boolean(row.querySelector("input[name=restricted]")?.checked)
  }))
  const checked = name => Boolean(form.querySelector(`input[name=${name}]`)?.checked)
  const value = name => form.elements[name]?.value ?? ""
  // A number field holding what isn't a number reads as "": say it's invalid instead.
  const number = name => (form.elements[name]?.validity?.badInput ? "invalid" : value(name))
  const autonomy = {}
  for (const row of form.querySelectorAll("[data-ws-autonomy]")) {
    const checked = row.querySelector("input[type=radio]:checked")
    if (checked) autonomy[row.dataset.wsAutonomy] = checked.value
  }
  const input = {
    departments,
    listed: form.querySelector("input[name=managers][value=listed]")?.checked,
    managers: [ ...form.querySelectorAll("input[name=duty_managers]:checked") ].map(input => input.value),
    policy: form.querySelector("input[name=confirm_policy]:checked")?.value,
    autonomy,
    hermesUserId: form.querySelector("input[name=hermes_fizzy_user_id]")?.value,
    visibility: {
      mode: form.querySelector("input[name=visibility_mode]:checked")?.value,
      untagged: form.querySelector("input[name=visibility_untagged]:checked")?.value
    },
    notifications: {
      enabled: checked("alerts_enabled"),
      severities: [ ...form.querySelectorAll("input[name=alert_severities]:checked") ].map(input => input.value),
      departmentRooms: checked("alert_department_rooms"),
      newReminder: number("new_reminder_min"),
      draftReminder: number("draft_reminder_min")
    },
    handover: {
      room: value("handover_room"),
      shiftEnds: value("shift_ends"),
      timeZone: value("time_zone"),
      reminder: checked("handover_reminder")
    }
  }
  const problem = logic.settingsProblem?.(input)
  if (problem) return showStatus(form, problem, "error")
  const body = logic.settingsBody(input)
  const submit = form.querySelector("[type=submit]")
  submit.disabled = true
  showStatus(form, "Saving…")
  try {
    await postJSON(form.action, body)
    showStatus(form, "Settings saved.", "ok")
  } catch (error) {
    showStatus(form, error.message, "error")
  } finally {
    submit.disabled = false
  }
}

// --- Handover (phase 2.6) ---------------------------------------------------------------------------

async function postHandover(form) {
  const text = form.elements.text?.value ?? ""
  const problem = logic.handoverProblem(text)
  if (problem) return showStatus(form, problem, "error")
  const submit = form.querySelector("[type=submit]")
  submit.disabled = true
  showStatus(form, "Posting…")
  try {
    const data = await postJSON(form.action, { text })
    showStatus(form, data.message || "Posted.", "ok")
  } catch (error) {
    submit.disabled = false
    showStatus(form, error.message, "error")
  }
}

// --- Wiring ----------------------------------------------------------------------------------------

// The whole document, not the body: Turbo swaps the body on every visit.
new MutationObserver(mutations => {
  const relevant = mutations.some(mutation => [ ...mutation.addedNodes ].some(node =>
    node.nodeType === Node.ELEMENT_NODE && !node.dataset.wsV &&
      (node.matches("a[data-ws-card], .ws-draft, .message, [data-ws-panel-root]") || node.querySelector("a[data-ws-card], .ws-draft, .message"))
  ))
  if (relevant) scheduleScan()
}).observe(document.documentElement, { childList: true, subtree: true })

document.addEventListener("turbo:load", () => scheduleScan(0))
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") scheduleScan(0) })
setInterval(() => { if (ready && document.visibilityState === "visible") { refreshChips(); refreshProposals() } }, REFRESH_MS)

scheduleScan(0)
