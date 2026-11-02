# Microcopy — the split voice in strings

The voice is split by design (BRAND.md §4): the admin UI is warm, calm, and
plain — a family surface. The README and marketing carry the opinionated
register. This guide pins down what each voice sounds like *in strings*, with
do/don't pairs pulled from real shipped copy in `web-app/src/`.

Neither voice mocks people (BRAND.md §2). Errors blame the situation, never
the user; success never celebrates the obvious.

## Rule of thumb

- **UI voice:** warm, plain, never cute. Say what happened and what to do next.
- **README voice:** wry, concrete, measured claims.
- If a string could appear in a family group chat without embarrassment, the UI
  voice is right. If it could survive a skeptical sysadmin's skim, the README
  voice is right.

## Empty states

The empty state is a first impression, not a gap. Name the thing, explain what
it is, offer the one action.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "No screens yet" + "A screen is one picture for your display — a clock, your calendar, a photo. Make one and it shows up here." | "Nothing here! 🎉" |
| "No displays yet" + "A display joins by pointing at this server: on the device, Settings → Server URL → this address." | "You have no devices. Go get some." |
| "Nothing queued" + "Add a screen below and it joins the rotation." | "Playlist is empty!!" |
| "No sources connected" + "A source pulls live information — your calendar or the weather — and feeds the extension screens below." | "No providers found. (╯°□°)╯" |

The pattern: **title = what's missing (plain), blurb = what the thing IS + how
it gets here, one primary action.** A definition, not a shrug.

## Errors

State the fact, state the consequence, never blame.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "Offline — blazonry server unreachable. Data shown may be stale." | "Oops! Something went wrong 😢" |
| "Saved, but the first fetch failed: …" | "Failed! (partially)" |
| "Refresh failed: …" (server's own words, passed through) | "Error 500" |
| "Rendering…" (while it works) | "Loading... please wait..." |

The offline banner is the model: *fact* (unreachable), *consequence* (data may
be stale), no apology, no emoji. Where the server's error message is already
plain English, pass it through verbatim instead of wrapping it.

## Destructive actions

Confirmations name the blast radius. "Delete?" is not a confirmation.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "Delete ALL firmware rows?" | "Are you sure???" |
| "Delete playlist (devices will be unassigned)?" | "Delete playlist?" |
| "Delete this screen (removed from all playlists)?" | "Really really delete?" |
| `Delete "${p.name}"? Extensions using it will stop updating.` | "Warning! This may have side effects!" |

The shipped pattern: **verb + object + parenthetical consequence.** The
parenthetical is the whole point — it's what a careful human would say out
loud before clicking.

## Status words

Statuses are derived facts, never moods.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "Active" / "Low battery" / "Offline" | "Rocking! 🚀 / Hmm / Dead" |
| "All displays healthy" (only when derived true) | "Woohoo! Everything works!" |
| "No poll since 2h ago · check device" | "Device gone missing 😱" |
| "Nothing to watch yet." (attention panel, no displays) | "It's so quiet in here…" |

"All displays healthy" is legal because it is *derived* — it only renders when
every display checks out. A cheer you can't verify is a lie with confetti.

## Button verbs

Buttons name the outcome, not the mechanism. Sentence case, verb first, no
"OK"/"Submit"/"Confirm" anywhere.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "Add display" / "New screen" / "New playlist" | "Click here to add" |
| "Create admin account" (first run) vs "Create account" | "Submit" |
| "Sign in with SSO" | "Login" (as a verb) |
| "Hide" / "Show" (playlist item visibility) | "Toggle" |
| "Reload" (update banner) | "Refresh page?" |

First-run copy distinguishes itself ("Create **admin** account") because the
first account *is* the admin — the button says so.

## Toasts

Success toasts are one word when one word is enough.

| ✓ Do (shipped) | ✗ Don't |
|---|---|
| "saved" / "deleted" / "verified" | "Your changes have been successfully saved!" |
| "Connected — 12 item(s) fetched" | "Success! We got your data!" |
| "Created — screen #7 added; add it to a playlist to show it" | "Done! 🎉" |

The last one is the full pattern: *what happened + the next thing you'll want
to know.* Lowercase toasts are deliberate — they read as system notes, not
shouts.

## What the README voice sounds like (for contrast)

| ✓ README voice (shipped) | ✗ Corporate drift |
|---|---|
| "One 13 MB binary, zero containers, no Chrome." | "A seamless, cloud-free display ecosystem." |
| "If you came here from 'why can't I point my display at my own calendar':" | "Are you tired of walled-garden solutions?" |

Same facts, no marketing heat. The README may be wry; it may not be smug.
