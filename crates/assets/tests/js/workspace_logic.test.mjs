// Hermes fork: tests of the Duty Manager Workspace's page logic (overrides/hermes/workspace_logic.js),
// with Node's built-in runner, no npm package: `node --test crates/assets/tests/js/*.test.mjs`.
import { test } from "node:test"
import assert from "node:assert/strict"

import * as logic from "../../overrides/hermes/workspace_logic.js"

test("the module also publishes itself for workspace.js", () => {
  assert.equal(globalThis.HermesWorkspace.hash, logic.hash)
  assert.equal(typeof globalThis.HermesWorkspace.settingsBody, "function")
})

test("chip versions are stable and tell different chips apart", () => {
  const chip = '<a class="ws-chip" data-ws-card="12">No. 12</a>'
  assert.equal(logic.hash(chip), logic.hash(chip))
  assert.notEqual(logic.hash(chip), logic.hash(chip.replace("12</a>", "13</a>")))
  assert.match(logic.hash(""), /^[0-9a-z]+$/)
  assert.equal(logic.hash(""), (5381).toString(36))
})

test("chip numbers: digits only, once each, bounded", () => {
  assert.deepEqual(logic.chipNumbers([ "12", "13", "12", "x1", "", "007" ]), [ "12", "13", "007" ])
  const many = Array.from({ length: 80 }, (_, i) => String(i))
  assert.equal(logic.chipNumbers(many).length, 50)
  assert.equal(logic.chipNumbers(many, 3).length, 3)
})

test("proposal ids are what the server accepts", () => {
  assert.deepEqual(logic.proposalIds([ "k3x9a01b", "k3x9a01b", "Bad", "a b", "", "x".repeat(41), "z9" ]), [ "k3x9a01b", "z9" ])
})

test("replies: a redirect or a 401 is a signed-out session, errors carry the server's message", () => {
  assert.equal(logic.replyProblem({ status: 200, redirected: true, ok: true }), logic.SIGNED_OUT)
  assert.equal(logic.replyProblem({ status: 401, redirected: false, ok: false }), logic.SIGNED_OUT)
  assert.equal(logic.replyProblem({ status: 403, ok: false }, { message: "Only duty managers can do this." }), "Only duty managers can do this.")
  assert.equal(logic.replyProblem({ status: 502, ok: false }, {}), "Something went wrong (HTTP 502).")
  assert.equal(logic.replyProblem({ status: 201, redirected: false, ok: true }, {}), null)
})

test("draft status lines", () => {
  assert.equal(logic.draftBusyText("confirm"), "Filing…")
  assert.equal(logic.draftBusyText("dismiss"), "Dismissing…")
  assert.equal(logic.draftSentText("confirm"), "Sent “confirm” to Hermes.")
  assert.equal(logic.draftSentText("dismiss"), "Sent “cancel” to Hermes.")
  assert.equal(logic.draftSentText("confirm", { message: "Card #14 filed." }, true), "Card #14 filed.")
  assert.equal(logic.draftSentText("dismiss", {}, true), "Dismissed.")
})

test("a proposal's state replaces its buttons once it isn't pending", () => {
  assert.equal(logic.proposalStateText({ status: "pending", label: "Waiting for a confirmation" }), null)
  assert.equal(logic.proposalStateText(undefined), null)
  assert.equal(logic.proposalStateText({ status: "done", label: "Confirmed by Karim" }), "Confirmed by Karim")
  assert.equal(logic.proposalStateText({ status: "expired" }), "expired")
})

test("sheet URLs keep the earlier comments shown", () => {
  assert.equal(logic.sheetUrl("/workspace/cards/12"), "/workspace/cards/12")
  assert.equal(logic.sheetUrl("/workspace/cards/12", { allComments: true }), "/workspace/cards/12?comments=all")
  assert.equal(logic.sheetUrl("/workspace/cards/12", { change: "comment" }), "/workspace/cards/12/comment")
  assert.equal(logic.sheetUrl("/workspace/cards/12", { change: "comment", allComments: true }), "/workspace/cards/12/comment?comments=all")
  assert.equal(globalThis.HermesWorkspace.sheetUrl, logic.sheetUrl)
})

test("card sheet changes", () => {
  assert.deepEqual(logic.changeBody("severity", { value: "high" }), { severity: "high" })
  assert.deepEqual(logic.changeBody("move", { value: "column:c1" }), { to: "column:c1" })
  assert.deepEqual(logic.changeBody("departments", { checkedValues: [ "engineering", "security" ] }), { tags: [ "engineering", "security" ] })
  assert.deepEqual(logic.changeBody("step", { stepId: "s1", checked: true }), { step_id: "s1", completed: true })
  assert.equal(logic.changeBody("fly", {}), null)
})

test("the settings form as the server reads it", () => {
  const body = logic.settingsBody({
    departments: [ { name: "Engineering", tag: "#Engineering", rooms: [ "3", "x", 7 ] } ],
    listed: true,
    managers: [ "5", "1" ],
    policy: "author_or_duty_manager",
    autonomy: { create: "ask_first", close: "never" },
    hermesUserId: "  03hermes ",
    visibility: { mode: "by_department_room", untagged: "duty_managers" },
    notifications: { enabled: true, severities: [ "critical" ], departmentRooms: false, newReminder: "20", draftReminder: "" },
    handover: { room: "12", shiftEnds: "06:00, 14:00 22:00", timeZone: " Europe/Paris ", reminder: true }
  })
  assert.deepEqual(body, {
    departments: [ { name: "Engineering", tag: "#Engineering", rooms: [ 3, 7 ], restricted: false } ],
    duty_managers: [ 5, 1 ],
    confirm_policy: "author_or_duty_manager",
    autonomy: { create: "ask_first", close: "never" },
    hermes_fizzy_user_id: "03hermes",
    visibility: { mode: "by_department_room", untagged: "duty_managers" },
    notifications: { enabled: true, severities: [ "critical" ], department_rooms: false, new_reminder_min: 20, draft_reminder_min: 10 },
    handover: { room_id: 12, shift_ends: [ "06:00", "14:00", "22:00" ], time_zone: "Europe/Paris", reminder: true }
  })
  const defaults = logic.settingsBody({ managers: [ "5" ] })
  assert.equal(defaults.duty_managers, null, "not listed: the administrators")
  assert.equal(defaults.confirm_policy, "anyone")
  assert.equal(defaults.hermes_fizzy_user_id, null)
  assert.deepEqual(defaults.visibility, { mode: "everyone", untagged: "everyone" })
  assert.equal(defaults.handover.room_id, null, "no room picked")
  assert.equal(logic.settingsBody({ handover: { room: "x" } }).handover.room_id, null)
  assert.equal(logic.settingsProblem({ notifications: { newReminder: "20", draftReminder: "" } }), null, "empty: the default")
  for (const bad of [ "-3", "2.5", "abc", "1441" ]) {
    const problem = logic.settingsProblem({ notifications: { newReminder: bad } })
    assert.match(problem, /whole number of minutes/, bad)
    assert.equal(logic.settingsBody({ notifications: { draftReminder: bad } }).notifications.draft_reminder_min, null, `${bad}: not a silent default`)
  }
  assert.equal(logic.settingsBody({ departments: [ { name: "S", tag: "s", restricted: "on" } ] }).departments[0].restricted, true)
})

test("shift ends are split on commas and spaces", () => {
  assert.deepEqual(logic.shiftEnds("07:00,15:00  23:00;"), [ "07:00", "15:00", "23:00" ])
  assert.deepEqual(logic.shiftEnds(""), [])
  assert.deepEqual(logic.shiftEnds(undefined), [])
})

test("with restricted visibility, chips the reply leaves out become plain links", () => {
  const reply = { visibility: "by_department_room", cards: { 12: "<a class=\"ws-chip\">…</a>" } }
  assert.deepEqual(logic.chipsToHide([ "12", "13", 13 ], reply), [ "13" ])
  assert.deepEqual(logic.chipsToHide([ "12", "13" ], { cards: {} }), [], "everyone: unknown cards keep what they show")
  assert.deepEqual(logic.chipsToHide([ "13" ], undefined), [])
})

test("a handover is checked before it's posted", () => {
  assert.equal(logic.handoverProblem("  \n "), "The handover is empty.")
  assert.equal(logic.handoverProblem(undefined), "The handover is empty.")
  assert.match(logic.handoverProblem("x".repeat(logic.MAX_HANDOVER_CHARS + 1)), /too long/)
  assert.equal(logic.handoverProblem("é".repeat(logic.MAX_HANDOVER_CHARS)), null, "characters, not bytes")
  assert.equal(logic.handoverProblem("Handover: all quiet."), null)
})
