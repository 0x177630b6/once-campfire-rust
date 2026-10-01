// Hermes fork: the Duty Manager Workspace's page logic that doesn't touch the page
// (docs/hermes-workspace.md), apart from hermes/workspace.js so that Node's own test runner can test
// it without a browser or any npm package: `node --test crates/assets/tests/js/*.test.mjs`.
//
// The tab bar loads this module before hermes/workspace.js (module scripts run in document order),
// which reads it from `globalThis.HermesWorkspace`: asset URLs are digested, so workspace.js can't
// import it by a relative path.

export const SIGNED_OUT = "You’re signed out. Sign in again, then retry."
export const UNREACHABLE = "The server can’t be reached. Check your connection and retry."
export const MAX_CHIPS = 50

// Tells a chip we put in from one the server rendered, so that swapping it in doesn't trigger
// another refresh (djb2-xor, base 36).
export function hash(text) {
  let h = 5381
  for (let i = 0; i < text.length; i++) h = ((h * 33) ^ text.charCodeAt(i)) >>> 0
  return h.toString(36)
}

// The card numbers of the chip links on the page: digits only, once each, at most `max`.
export function chipNumbers(values, max = MAX_CHIPS) {
  return [ ...new Set(values) ].filter(n => /^\d+$/.test(String(n))).slice(0, max)
}

// Proposal ids of the drafts on the page, once each (what the server accepts: [a-z0-9]{1,40}).
export function proposalIds(values, max = 50) {
  return [ ...new Set(values) ].filter(id => /^[a-z0-9]{1,40}$/.test(String(id))).slice(0, max)
}

// What went wrong with a JSON reply, or null when it's fine. A redirect or a 401 means the session
// expired: the session check redirects to the sign-in page, which fetch follows (200, HTML).
export function replyProblem({ status, redirected, ok }, body = {}) {
  if (redirected || status === 401) return SIGNED_OUT
  if (ok === false || status >= 400) return body?.message || `Something went wrong (HTTP ${status}).`
  return null
}

// A draft's status line while its answer is on its way.
export function draftBusyText(decision) {
  return decision === "confirm" ? "Filing…" : "Dismissing…"
}

// A draft's status line once answered: a Hermes proposal says what the server did ("Card #14
// filed."); a text draft says what was posted in the room.
export function draftSentText(decision, body = {}, proposal = false) {
  if (proposal) return body?.message || (decision === "confirm" ? "Done." : "Dismissed.")
  return decision === "confirm" ? "Sent “confirm” to Sky." : "Sent “cancel” to Sky."
}

// A proposal's state from GET /workspace/hermes/proposals.json: null while it waits (the buttons
// stay), else the line to show instead of them.
export function proposalStateText(state) {
  if (!state || state.status === "pending") return null
  return state.label || state.status
}

// The body of a card sheet's change (POST /workspace/cards/:n/<kind>), from the control's values.
export function changeBody(kind, { value, checked, checkedValues = [], stepId } = {}) {
  switch (kind) {
    case "severity": return { severity: value }
    case "move": return { to: value }
    case "departments": return { tags: [ ...checkedValues ] }
    case "step": return { step_id: stepId, completed: Boolean(checked) }
  }
  return null
}

// "07:00, 15:00 23:00" → [ "07:00", "15:00", "23:00" ] (the server validates them).
export function shiftEnds(text) {
  return String(text || "").split(/[\s,;]+/).map(value => value.trim()).filter(Boolean)
}

export const MAX_REMINDER_MINUTES = 24 * 60

// A whole number of minutes from a number field: empty → `fallback`, invalid → null (which the
// server refuses: never a silent default; `settingsProblem` says what's wrong before posting).
function minutes(value, fallback) {
  const text = String(value ?? "").trim()
  if (text === "") return fallback
  const number = Number(text)
  return /^\d+$/.test(text) && number <= MAX_REMINDER_MINUTES ? number : null
}

// What's wrong with the settings form before posting it, or null (the server checks the rest).
export function settingsProblem({ notifications = {} } = {}) {
  const fields = [ [ notifications.newReminder, "“Still in New” reminder" ], [ notifications.draftReminder, "Waiting proposal reminder" ] ]
  for (const [ value, label ] of fields) {
    if (minutes(value, 0) === null) {
      return `${label}: give a whole number of minutes, 0 to ${MAX_REMINDER_MINUTES} (0 turns it off).`
    }
  }
  return null
}

// The settings form as POST /workspace/settings reads it. `autonomy` maps action → dial; ids are
// numbers. Phase 2.5–2.7: `visibility`, `notifications`, `handover`.
export function settingsBody({
  departments = [], listed = false, managers = [], policy, autonomy = {}, hermesUserId = "",
  visibility = {}, notifications = {}, handover = {}
} = {}) {
  const room = Number(handover.room)
  return {
    departments: departments.map(department => ({
      name: department.name,
      tag: department.tag,
      rooms: (department.rooms || []).map(Number).filter(Number.isFinite),
      restricted: Boolean(department.restricted)
    })),
    duty_managers: listed ? managers.map(Number).filter(Number.isFinite) : null,
    confirm_policy: policy || "anyone",
    autonomy: { ...autonomy },
    hermes_fizzy_user_id: String(hermesUserId || "").trim() || null,
    visibility: { mode: visibility.mode || "everyone", untagged: visibility.untagged || "everyone" },
    notifications: {
      enabled: Boolean(notifications.enabled),
      severities: [ ...(notifications.severities || []) ],
      department_rooms: Boolean(notifications.departmentRooms),
      new_reminder_min: minutes(notifications.newReminder, 15),
      draft_reminder_min: minutes(notifications.draftReminder, 10)
    },
    handover: {
      room_id: String(handover.room ?? "").trim() !== "" && Number.isInteger(room) && room > 0 ? room : null,
      shift_ends: shiftEnds(handover.shiftEnds),
      time_zone: String(handover.timeZone || "").trim(),
      reminder: Boolean(handover.reminder)
    }
  }
}

// Phase 2.7: with visibility restricted, `cards.json` only has the cards the viewer may see. The
// chips on the page whose number isn't in the reply (a card now hidden from them, in a page
// rendered before) become plain links again. Otherwise nothing changes.
export function chipsToHide(chipNumbers, reply = {}) {
  if (reply?.visibility !== "by_department_room") return []
  const cards = reply?.cards || {}
  return [ ...new Set(chipNumbers.map(String)) ].filter(number => !(number in cards))
}

export const MAX_HANDOVER_CHARS = 10_000

// What's wrong with a handover's text before posting it, or null.
export function handoverProblem(text) {
  const value = String(text ?? "")
  if (!value.trim()) return "The handover is empty."
  if ([ ...value ].length > MAX_HANDOVER_CHARS) return `The handover is too long (${MAX_HANDOVER_CHARS} characters at most).`
  return null
}

// A card sheet's URL (its action URL, e.g. /workspace/cards/12), for a change (`/<change>`) or a
// reload, keeping its earlier comments shown when they are (`data-ws-comments="all"`).
export function sheetUrl(actionUrl, { change = "", allComments = false } = {}) {
  return `${actionUrl}${change ? `/${change}` : ""}${allComments ? "?comments=all" : ""}`
}

globalThis.HermesWorkspace = {
  SIGNED_OUT, UNREACHABLE, MAX_CHIPS, hash, chipNumbers, proposalIds, replyProblem, draftBusyText, draftSentText,
  proposalStateText, changeBody, settingsBody, settingsProblem, MAX_REMINDER_MINUTES, shiftEnds, chipsToHide, MAX_HANDOVER_CHARS, handoverProblem,
  sheetUrl
}
