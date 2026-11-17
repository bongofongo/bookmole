-- bookmole's KOReader plugin.
--
-- The *link*: bookmole installs this directory over USB and writes
-- `pairing.lua` beside this file, so the reader can say who it is paired with.
--
-- `pairing.lua` is a **list**. A reader can be paired with more than one
-- computer, and when the file held a single entry (plugin version 1) the
-- second bookmole install silently overwrote the first one's — so this
-- plugin has to be able to name which computer it means before it can push to
-- one. Two shapes are read: the list under `computers`, and version 1's flat
-- single entry, which is a one-element list. Detected by shape, because a
-- version number nothing branches on is decoration.
--
-- Over the network there are two verbs, both started by the user: *push*
-- (the reader sends to a computer) and *pull* (the reader opens a short window
-- and a computer fetches). **Push is a tap and never a hook**: the user
-- opens Tools → bookmole → Push to <computer>, and nothing happens by
-- itself. `onAnnotationsModified` and `onCloseDocument` exist and are
-- deliberately not used — wifi is off by default and comes up transiently, so
-- "push the moment a highlight is made" really means "push next time the radio
-- is up", which is a different feature with a different failure mode.
--
-- No `pairing.lua` entry carries a host or a port. A missing endpoint is *not
-- configured*, and the plugin then does nothing at all: it **fails closed**,
-- and never blocks or slows the reader's UI, and this is that rule in its
-- degenerate case. What the reader
-- *learns* lands in `endpoint.lua` — written by this plugin, never by the
-- installer, never hashed into `installed.lua`, and removed by uninstall — and
-- the rule stays: cannot reach the app → say so once, and never block the UI.
--
-- The token never crosses the wire. It keys an HMAC-SHA256 challenge, and the
-- key is the **64 hex characters as `pairing.lua` holds them** — not the 32
-- bytes they encode. `ffi/sha2` computes correct HMAC either way and the two
-- answers differ completely with neither side looking wrong, so the choice is
-- written down here and in bookmole's `crates/engine/src/wireless.rs`,
-- whose tests pin a vector computed by `ffi/sha2` itself.
--
-- Version 3 lets a computer be **linked over wifi**, with no cable. The reader
-- shows a sixteen-character code and the person types it on the computer; the
-- code keys the same HMAC, so it is the one secret and it is carried by a
-- human, never by the network. Both sides then *derive* the long-lived token
-- from it, which keeps "the token never crosses the wire" true for a pairing
-- that never saw a cable. That makes this plugin a second writer of
-- `pairing.lua`, so `serialisePairings` produces exactly the bytes the
-- installer does — and the same pass lets it remember **several addresses**
-- for one computer (a laptop has a home and an office), including ones typed
-- in by hand for the networks where nothing can be discovered.

local InfoMessage = require("ui/widget/infomessage")
local UIManager = require("ui/uimanager")
local WidgetContainer = require("ui/widget/container/widgetcontainer")
local logger = require("logger")
local T = require("ffi/util").template
local _ = require("gettext")

-- The fixed rendezvous port. A constant on both sides, and checked rather than
-- picked: IANA has no assignment at or above 61000, and Linux's default
-- ephemeral range stops at 60999.
local RENDEZVOUS_PORT = 61862
local PROTOCOL_VERSION = 1

local Bookmole = WidgetContainer:extend{
    name = "bookmole",
    is_doc_only = false,
}

-- Normalise whatever `pairing.lua` returned into a list of computers.
--
-- A **pure function of a table**, deliberately: it is the only logic in this
-- plugin worth being wrong about, and a function that reaches for `self` or for
-- a `require` is one no test can execute. `crates/engine/src/plugin.rs`'s
-- `the_plugin_reads_every_pairing_shape_the_installer_can_write` runs exactly
-- this function under mlua against exactly the bytes the installer writes.
--
-- Entries missing an id or a token are dropped rather than repaired: half a
-- credential proves nothing, and an entry that cannot answer a challenge is a
-- menu line that fails after the user taps it.
function Bookmole.normalisePairings(raw)
    if type(raw) ~= "table" then
        return {}
    end
    local entries = raw.computers
    if type(entries) ~= "table" then
        -- Version 1's flat shape: one computer, written before there was a list.
        entries = { raw }
    end
    local out = {}
    for _, entry in ipairs(entries) do
        if type(entry) == "table"
            and type(entry.device_id) == "string" and entry.device_id ~= ""
            and type(entry.token) == "string" and entry.token ~= "" then
            table.insert(out, entry)
        end
    end
    return out
end

-- What to call a computer in the menu.
--
-- The name the installer wrote, and otherwise the first bytes of the id — never
-- "unknown", because the id is a real handle the user can match against what
-- bookmole shows on the desktop.
function Bookmole.computerName(entry)
    if type(entry.name) == "string" and entry.name ~= "" then
        return entry.name
    end
    return tostring(entry.device_id):sub(1, 8)
end

-- ---- `pairing.lua`, written from this side ----------------------------------

-- `s` as a Lua string literal, **exactly as the installer's `lua_quote` writes
-- one** (`crates/engine/src/plugin.rs`). Not `%q`: that escapes a newline as a
-- backslash and a real line break, which reads back the same and is different
-- bytes — and two writers of one file that disagree about bytes rewrite each
-- other's work on every connection.
function Bookmole.luaQuote(s)
    local quoted = tostring(s):gsub('[%c"\\]', function(c)
        if c == '"' then return '\\"' end
        if c == "\\" then return "\\\\" end
        if c == "\n" then return "\\n" end
        if c == "\r" then return "\\r" end
        return string.format("\\%03d", c:byte())
    end)
    return '"' .. quoted .. '"'
end

-- The source text of a `pairing.lua`, byte-for-byte what the installer's
-- `write_pairings` produces for the same list. The installer reads this file
-- back over the cable and rewrites its own entry, so the two must agree — the
-- round trip is asserted in `plugin.rs` against this function under mlua.
function Bookmole.serialisePairings(pairings)
    local lines = {
        "-- Written by bookmole: by the installer over USB, and by the plugin",
        "-- on this device when a computer is linked or unlinked over wifi. Do not",
        "-- edit.",
        "--",
        "-- A *list*, because a reader can be paired with more than one computer.",
        "-- Each entry is one computer: the id this reader has to that computer,",
        "-- the secret it proves itself with, and a name to show in the menu. A",
        "-- writer changes its own entry and leaves every other one alone.",
        "return {",
        "    computers = {",
    }
    for _i, c in ipairs(pairings) do
        table.insert(lines, "        {")
        table.insert(lines, "            device_id = " .. Bookmole.luaQuote(c.device_id) .. ",")
        table.insert(lines, "            token     = " .. Bookmole.luaQuote(c.token) .. ",")
        if type(c.name) == "string" and c.name ~= "" then
            table.insert(lines, "            name      = " .. Bookmole.luaQuote(c.name) .. ",")
        end
        table.insert(lines, string.format("            paired_at = %d,", tonumber(c.paired_at) or 0))
        table.insert(lines, "        },")
    end
    table.insert(lines, "    },")
    table.insert(lines, "}")
    table.insert(lines, "")
    return table.concat(lines, "\n")
end

-- `entry` put into `pairings`: in place when its id is already there, so the
-- menu's order does not shuffle when a computer is linked again, and at the end
-- otherwise. A new list; the argument is not touched.
function Bookmole.upsertPairing(pairings, entry)
    local out, placed = {}, false
    for _i, c in ipairs(pairings) do
        if c.device_id == entry.device_id then
            table.insert(out, entry)
            placed = true
        else
            table.insert(out, c)
        end
    end
    if not placed then
        table.insert(out, entry)
    end
    return out
end

function Bookmole.removePairing(pairings, device_id)
    local out = {}
    for _i, c in ipairs(pairings) do
        if c.device_id ~= device_id then
            table.insert(out, c)
        end
    end
    return out
end

-- ---- the discovery ladder, as a pure function ------------------------------

-- The addresses to try, in order, for one computer.
--
-- **Pure**, and that is the point: this is the part with bugs in it, and a
-- ladder that can only be exercised by moving a laptop between subnets is a
-- ladder with no tests. `crates/engine/src/plugin.rs` runs exactly this under
-- mlua. The rungs, and what each one is for:
--
--   1. every **known address**, most recently reached first, straight from
--      `endpoint.lua`. The common case: a renewed DHCP lease is usually the
--      same address — and a laptop that lives on two networks has two of them,
--      which is why this is a list. One that was typed in as a *name* is
--      resolved first, like rung 4.
--   2. **broadcast** to 255.255.255.255. Wins when the address changed and the
--      subnet did not. `plugins/calibre.koplugin/wireless.lua` does exactly
--      this shape, so it is known to work on these devices.
--   3. **directed broadcast** to each known /24's .255, for the drivers and
--      APs that pass 192.0.2.255 while filtering 255.255.255.255.
--   4. the **hostname**, resolved by DNS. The best rung for a roaming laptop:
--      most consumer routers register DHCP hostnames, so a machine that
--      changes address keeps its name — and no code of ours goes on the wire.
--
-- No host is tried twice, whichever rungs would have named it.
--
-- mDNS is deliberately absent. KOReader has none anywhere, so it would be a
-- hundred lines of hand-rolled DNS packet code, and e-reader wifi power-save
-- drops multicast often. It is held in reserve, not built first.
function Bookmole.ladder(addresses, name)
    local rungs, tried = {}, {}
    local function add(kind, host, port)
        if not tried[host] then
            tried[host] = true
            table.insert(rungs, { kind = kind, host = host, port = port })
        end
    end
    local subnets = {}
    if type(addresses) == "table" then
        for _i, a in ipairs(addresses) do
            if type(a) == "table" and type(a.host) == "string" and a.host ~= "" then
                -- The /24 an address sat on. Only ever derived from a dotted
                -- quad: a hostname has no subnet to broadcast into, and
                -- `udp:sendto` takes an address and not a name, so it is a
                -- `dns` rung and `discover` resolves it.
                local x, y, z = a.host:match("^(%d+)%.(%d+)%.(%d+)%.%d+$")
                if x then
                    add("cached", a.host, a.port)
                    table.insert(subnets, x .. "." .. y .. "." .. z .. ".255")
                else
                    add("dns", a.host)
                end
            end
        end
    end
    add("broadcast", "255.255.255.255")
    for _i, subnet in ipairs(subnets) do
        add("broadcast", subnet)
    end
    if type(name) == "string" and name ~= "" and not name:match("^%d+%.%d+%.%d+%.%d+$") then
        -- Bare first, because a router that registers the short name usually
        -- also answers it; the suffixes are for the ones that need them.
        for _i, suffix in ipairs({ "", ".lan", ".home.arpa" }) do
            add("dns", name .. suffix)
        end
    end
    return rungs
end

-- How many *learned* addresses one computer keeps. A home, an office and
-- somewhere else; past that the oldest is a network this reader has left, and
-- every stale address is a second of e-ink waiting on a probe nobody answers.
-- An address a person typed in is never dropped to make room.
local LEARNED_LIMIT = 3

-- What `endpoint.lua` returned, normalised: for each computer, **a list** of
-- addresses in the order they should be tried. Absent, unreadable and empty
-- are all the same answer, and the answer is a table — never a nil the caller
-- has to test for.
--
-- Two shapes are read. Version 2 kept one address per computer, as a flat
-- `{ host, port, seen_at }`; that is a one-element list, detected by shape for
-- `normalisePairings`' reason.
function Bookmole.parseEndpoints(raw)
    if type(raw) ~= "table" then
        return {}
    end
    local function address(entry)
        if type(entry) == "table" and type(entry.host) == "string" and entry.host ~= "" then
            return {
                host = entry.host,
                port = tonumber(entry.port),
                seen_at = tonumber(entry.seen_at),
                typed = entry.typed == true or nil,
            }
        end
    end
    local out = {}
    for device_id, entry in pairs(raw) do
        if type(device_id) == "string" and type(entry) == "table" then
            -- `table.insert(list, nil)` is harmless and `table.insert(list)`
            -- throws — and a function that falls off its end returns *nothing*,
            -- not nil. So the value is named before it is inserted.
            local list = {}
            local candidates = type(entry.host) == "string" and { entry } or entry
            for _i, one in ipairs(candidates) do
                local parsed = address(one)
                if parsed then
                    table.insert(list, parsed)
                end
            end
            if #list > 0 then
                out[device_id] = list
            end
        end
    end
    return out
end

-- The source text of an `endpoint.lua`. Written by us and by the installer —
-- which stamps the computer's own address over the cable — so the bytes are
-- pinned against `write_endpoints` in `crates/engine/src/plugin.rs`. Never
-- hashed: see `UNHASHED_FILES` there for why the installer has to know about
-- this file at all.
function Bookmole.serialiseEndpoints(endpoints)
    local lines = {
        "-- Written by the bookmole plugin on this device.",
        "-- Where each paired computer has been reached, most recent first, and",
        "-- any address typed in by hand. Safe to delete: discovery rebuilds what",
        "-- it learned, and a typed address can be typed again.",
        "return {",
    }
    -- Sorted, so a file rewritten with the same content has the same bytes and
    -- a person diffing two readers sees only what differs. The addresses
    -- *within* a computer are not sorted: their order is the order to try.
    local ids = {}
    for id in pairs(endpoints) do
        table.insert(ids, id)
    end
    table.sort(ids)
    for _i, id in ipairs(ids) do
        if #endpoints[id] > 0 then
            table.insert(lines, "    [" .. Bookmole.luaQuote(id) .. "] = {")
            for _j, e in ipairs(endpoints[id]) do
                table.insert(lines, string.format(
                    "        { host = %s, port = %d, seen_at = %d%s },",
                    Bookmole.luaQuote(e.host), e.port or 0, e.seen_at or 0,
                    e.typed and ", typed = true" or ""))
            end
            table.insert(lines, "    },")
        end
    end
    table.insert(lines, "}")
    table.insert(lines, "")
    return table.concat(lines, "\n")
end

-- A computer answered at `host`: that address moves to the front, and the
-- learned ones past `LEARNED_LIMIT` fall off the end. A new list.
function Bookmole.rememberAddress(addresses, host, port, now)
    local front = { host = host, port = port, seen_at = now }
    for _i, a in ipairs(addresses or {}) do
        -- The same address, reached again. It keeps having been typed.
        if a.host == host and a.typed then
            front.typed = true
        end
    end
    local out = { front }
    local learned = front.typed and 0 or 1
    for _i, a in ipairs(addresses or {}) do
        if a.host ~= host then
            if a.typed then
                table.insert(out, a)
            elseif learned < LEARNED_LIMIT then
                learned = learned + 1
                table.insert(out, a)
            end
        end
    end
    return out
end

-- An address a person typed in. Tried first, because they have just told us
-- where the computer is; kept until they remove it.
function Bookmole.addTypedAddress(addresses, host)
    local out = { { host = host, port = 0, seen_at = 0, typed = true } }
    for _i, a in ipairs(addresses or {}) do
        if a.host == host then
            out[1].port, out[1].seen_at = a.port, a.seen_at
        else
            table.insert(out, a)
        end
    end
    return out
end

function Bookmole.removeAddress(addresses, host)
    local out = {}
    for _i, a in ipairs(addresses or {}) do
        if a.host ~= host then
            table.insert(out, a)
        end
    end
    return out
end

-- What a person typed, as an address — or nil. Letters, digits, dots and
-- hyphens: enough for `192.0.2.63` and for `desk.lan`, and nothing that
-- could be anything else by the time it is written into a Lua file.
function Bookmole.validAddress(text)
    if type(text) ~= "string" then
        return nil
    end
    local host = text:match("^%s*(.-)%s*$")
    if host == "" or #host > 253 or not host:match("^[%w][%w%.%-]*$") then
        return nil
    end
    return host
end

-- `self.path` is set by `pluginloader.lua` to this directory. Our own state
-- lives inside it rather than in `DataStorage:getSettingsDir()` where kosync
-- keeps its credentials: bookmole promises that uninstalling is exact —
-- one directory removed, nothing left behind — and a token in the settings
-- directory would quietly break that promise.
function Bookmole:readPairings()
    local ok, raw = pcall(dofile, self.path .. "/pairing.lua")
    if not ok then
        return {}
    end
    return Bookmole.normalisePairings(raw)
end

function Bookmole:readEndpoints()
    local ok, raw = pcall(dofile, self.path .. "/endpoint.lua")
    if not ok then
        return {}
    end
    return Bookmole.parseEndpoints(raw)
end

-- Write one of our two files, whole. Returns whether it landed.
--
-- **Directly, with no temporary file beside it.** A rename would be the safer
-- write, and it is the wrong one here: a stray `pairing.lua.tmp` left by a
-- device that lost power is a file the installer does not recognise, and the
-- installer refuses to install *or uninstall* around one of those.
function Bookmole:writeFile(name, body)
    local f = io.open(self.path .. "/" .. name, "w")
    if not f then
        logger.dbg("bookmole: could not write", name)
        return false
    end
    local ok = f:write(body)
    f:close()
    return ok ~= nil
end

-- Replace the list of computers, on disk and in memory. The file first: a
-- computer the menu shows and the file does not would vanish on restart.
function Bookmole:savePairings(pairings)
    if not self:writeFile("pairing.lua", Bookmole.serialisePairings(pairings)) then
        return false
    end
    self.pairings = pairings
    return true
end

function Bookmole:saveEndpoints(endpoints)
    return self:writeFile("endpoint.lua", Bookmole.serialiseEndpoints(endpoints))
end

-- Remember where a computer actually answered.
--
-- Best-effort by design: a read-only filesystem or a full device is not a
-- reason to fail a push that worked. The cost of losing this file is one extra
-- broadcast next time.
function Bookmole:rememberEndpoint(device_id, host, port)
    local endpoints = self:readEndpoints()
    endpoints[device_id] = Bookmole.rememberAddress(endpoints[device_id], host, port, os.time())
    self:saveEndpoints(endpoints)
end

function Bookmole:init()
    self.pairings = self:readPairings()
    logger.dbg("bookmole: paired with", #self.pairings, "computer(s)")
    self.ui.menu:registerToMainMenu(self)
end

-- ---- the wire -------------------------------------------------------------

local function hmac(token, message)
    local sha = require("ffi/sha2")
    -- `sha.hmac(hash_func, key, message)` — the hash *function* first, and the
    -- key is the token's hex characters. Returns lowercase hex.
    return sha.hmac(sha.sha256, token, message)
end

-- The same function, reachable from a test: the pairing exchange below is run
-- under mlua against the desktop's own implementation, and this is the seam the
-- real HMAC goes in through.
Bookmole.hmac = hmac

local function nonce()
    -- Not cryptographic randomness, and it does not need to be: the nonce is a
    -- freshness token the far side records, never a secret. What must not
    -- repeat within one open door is the pair, and the clock plus a counter
    -- gives that on a device with one process.
    Bookmole._nonce_seq = (Bookmole._nonce_seq or 0) + 1
    return string.format("%d-%d", os.time(), Bookmole._nonce_seq)
end

-- ---- linking a computer over wifi: the pure half ----------------------------
--
-- The reader shows a code; the person types it into bookmole on the
-- computer. **The code is the secret, and a human carries it** — it is never
-- sent, in any form. What crosses the network is two datagrams, each signed
-- with it:
--
--     computer → reader   PAIR    { nonce, at, name, known, mac }
--     reader → computer   PAIRED  { nonce, device_id, reader, plugin, mac }
--
-- and then both sides compute the long-lived token as an HMAC over the two
-- nonces and the id. So the token never crosses the wire either, which is the
-- rule the cable pairing set and the reason this is not "send the token,
-- encrypted somehow".
--
-- Sixteen characters of a 32-symbol alphabet is 80 bits. That is the whole
-- defence against somebody recording the exchange and guessing codes at home,
-- and it is why the code is shown *here* and typed *there*: a keyboard makes
-- sixteen characters cheap, and an e-ink one would have forced six.

-- Crockford's alphabet: no I, L, O or U, so nothing on screen can be read two
-- ways, and the computer folds a typed O to 0 and I or L to 1.
local CODE_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
local CODE_LENGTH = 16

-- A code from random bytes: five bits of each. 256 is a multiple of 32, so
-- taking a byte modulo 32 favours no symbol.
function Bookmole.codeFromBytes(bytes)
    if type(bytes) ~= "string" or #bytes < CODE_LENGTH then
        return nil
    end
    local out = {}
    for i = 1, CODE_LENGTH do
        local n = bytes:byte(i) % 32
        out[i] = CODE_ALPHABET:sub(n + 1, n + 1)
    end
    return table.concat(out)
end

-- The code as it is shown: four groups of four. The hyphens are for the eye
-- and are not part of the secret — the computer drops them.
function Bookmole.formatCode(code)
    return (code:gsub("(....)", "%1-"):gsub("%-$", ""))
end

function Bookmole.hexFromBytes(bytes)
    return (bytes:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end

-- The three strings the exchange signs. `crates/engine/src/wireless.rs` builds
-- the same three with `format!`, and a test runs both and compares them.
function Bookmole.pairChallenge(pair_nonce, at, known, name)
    return string.format("pair:%d:%s:%d:%s:%s",
        PROTOCOL_VERSION, pair_nonce, at, table.concat(known, ","), name)
end

function Bookmole.pairedChallenge(pair_nonce, reader_nonce, device_id, plugin, reader)
    return string.format("paired:%d:%s:%s:%s:%d:%s",
        PROTOCOL_VERSION, pair_nonce, reader_nonce, device_id, plugin, reader)
end

function Bookmole.tokenChallenge(pair_nonce, reader_nonce, device_id)
    return string.format("token:%d:%s:%s:%s",
        PROTOCOL_VERSION, pair_nonce, reader_nonce, device_id)
end

-- Signed with the token this reader *already holds* for a computer, when it
-- answers as a reader that computer knows. The code proves somebody is holding
-- this device; only the old token proves it is the same device as before — and
-- without that, anything shown a code could answer with another reader's id
-- and be handed its place.
function Bookmole.rekeyChallenge(pair_nonce, reader_nonce, device_id)
    return string.format("rekey:%d:%s:%s:%s",
        PROTOCOL_VERSION, pair_nonce, reader_nonce, device_id)
end

-- Answer one `PAIR`, or do not.
--
-- **Pure** — the HMAC and the randomness come in as arguments — because this is
-- the half of the exchange a wrong byte breaks silently, and the desktop's test
-- suite runs it under mlua against the desktop's own half.
--
-- `state` is one open code: `{ code, pairings, plugin, reader }`, and after a
-- link `linked`. `mint()` returns a fresh nonce and a fresh device id. Returns
-- the reply to send and, **the first time only**, the pairing to store.
--
-- Silence is the answer to everything that is not a correctly signed `PAIR`:
-- a wrong code and a stranger's broadcast look the same from outside, and
-- neither learns that a reader is here.
function Bookmole.answerPair(state, msg, hmac_fn, mint)
    if type(msg) ~= "table" or msg.v ~= PROTOCOL_VERSION or msg.kind ~= "pair" then
        return nil
    end
    if type(msg.nonce) ~= "string" or msg.nonce == ""
        or type(msg.name) ~= "string"
        or type(msg.at) ~= "number" or msg.at ~= math.floor(msg.at)
        or type(msg.known) ~= "table"
        or type(msg.mac) ~= "string" then
        return nil
    end
    local known = {}
    for _i, id in ipairs(msg.known) do
        if type(id) ~= "string" then
            return nil
        end
        table.insert(known, id)
    end
    local want = hmac_fn(state.code,
        Bookmole.pairChallenge(msg.nonce, msg.at, known, msg.name))
    if want ~= msg.mac then
        return nil
    end

    -- **One code links one computer.** The computer repeats its `PAIR` until it
    -- hears us, so the same nonce gets the same answer; a different one, once
    -- this code has been used, gets none.
    if state.linked then
        if state.linked.nonce == msg.nonce then
            return state.linked.reply
        end
        return nil
    end

    -- A computer that already knows this reader says which ids it holds, and
    -- one of them being ours means this is that computer again. It keeps its
    -- id and its place in the menu and gets a new secret, rather than becoming
    -- a second entry with the same name and a dead one beside it.
    local reader_nonce, fresh_id = mint()
    local device_id, paired_at, held = fresh_id, msg.at, nil
    for _i, entry in ipairs(state.pairings or {}) do
        for _j, id in ipairs(known) do
            if entry.device_id == id and device_id == fresh_id then
                device_id = id
                paired_at = tonumber(entry.paired_at) or msg.at
                held = entry.token
            end
        end
    end

    local reply = {
        v = PROTOCOL_VERSION,
        kind = "paired",
        nonce = reader_nonce,
        device_id = device_id,
        reader = state.reader,
        plugin = state.plugin,
        mac = hmac_fn(state.code, Bookmole.pairedChallenge(
            msg.nonce, reader_nonce, device_id, state.plugin, state.reader)),
    }
    if held then
        -- The computer will not replace a secret it holds on the code's word
        -- alone. See `rekeyChallenge`.
        reply.proof = hmac_fn(held,
            Bookmole.rekeyChallenge(msg.nonce, reader_nonce, device_id))
    end
    state.linked = { nonce = msg.nonce, reply = reply }
    return reply, {
        device_id = device_id,
        token = hmac_fn(state.code,
            Bookmole.tokenChallenge(msg.nonce, reader_nonce, device_id)),
        name = msg.name ~= "" and msg.name or nil,
        paired_at = paired_at,
    }
end

-- ---- the Kindle's firewall -------------------------------------------------
--
-- **A Kindle drops the answer to a broadcast probe, and this is not a guess.**
-- Its INPUT policy is DROP, with a per-interface conntrack accept:
--
--     -P INPUT DROP
--     -A INPUT -i wlan0 -p udp -m state --state ESTABLISHED -j ACCEPT
--
-- A datagram sent to 255.255.255.255 makes a conntrack entry whose reply tuple
-- is *from* 255.255.255.255, so a unicast `HERE` from one computer does not
-- match it, arrives as NEW, and meets the policy. A unicast probe's answer does
-- match, and passes. Measured on the device, the two rungs seconds apart: the
-- broadcast timed out, the unicast was answered, and the desktop's own log
-- showed it had replied to *both*.
--
-- So the firewall is not only pull's problem, as it first appears. Discovery
-- is a UDP request/**reply**, the reply is inbound, and push needs the hole
-- first.
--
-- `SSH.koplugin` and `httpinspector.koplugin` both punch exactly this, and the
-- rules below are theirs. Two rather than one: this device has `-P OUTPUT
-- ACCEPT`, so the second is inert here, but it is what plugins known to work on
-- Kindle firmware do, and this is not the place to be inventive.

-- The rule, as a string. **Pure**, so a test can assert the exact command with
-- no Kindle to run it on — and `nil` for anything that is not a port, which is
-- what keeps a nil from becoming `--dport nil` on somebody's device.
function Bookmole.firewallRule(action, chain, proto, port)
    port = tonumber(port)
    if not port or port <= 0 or port > 65535 then return nil end
    if action ~= "A" and action ~= "D" then return nil end
    if proto ~= "tcp" and proto ~= "udp" then return nil end
    if chain == "INPUT" then
        return string.format(
            "iptables -%s INPUT -p %s --dport %d -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT",
            action, proto, port)
    elseif chain == "OUTPUT" then
        return string.format(
            "iptables -%s OUTPUT -p %s --sport %d -m conntrack --ctstate ESTABLISHED -j ACCEPT",
            action, proto, port)
    end
    return nil
end

-- Open or close a hole, on a Kindle and nowhere else.
--
-- Every other device KOReader runs on either has nothing in the way or is not
-- ours to reconfigure, which is the line `SSH.koplugin` draws too.
local function firewall(action, proto, port)
    local Device = require("device")
    if not Device.isKindle or not Device:isKindle() then
        return
    end
    for _, chain in ipairs({ "INPUT", "OUTPUT" }) do
        local rule = Bookmole.firewallRule(action, chain, proto, port)
        if rule then
            os.execute(rule)
        end
    end
end

-- Rung 1/3/4 of the ladder: ask one address and wait for a `HERE`.
--
-- The shape is `plugins/calibre.koplugin/wireless.lua`'s `find_calibre_server`
-- — `socket.udp4()`, `setoption("broadcast")`, `setsockname("*", port)`,
-- `sendto`, `receivefrom`, a short timeout — because that is code known to work
-- on these devices and inventing a second dialect of it would be guessing.
--
-- **The reply is verified before it is believed.** A rogue on a café LAN that
-- answers first cannot make this reader send a highlight: the MAC covers the
-- nonce *and the announced port*, so a captured reply cannot even be
-- re-advertised pointing somewhere else.
local function ask(udp, entry, host)
    local json = require("json")
    local n = nonce()
    local hello = json.encode({ v = PROTOCOL_VERSION, device_id = entry.device_id, nonce = n })
    local _, err = udp:sendto(hello, host, RENDEZVOUS_PORT)
    if err then
        logger.dbg("bookmole: could not send to", host, err)
        return nil
    end
    -- More than one answer is ordinary on a broadcast: other bookmoles are
    -- entitled to be on the LAN. Keep reading until one proves the token.
    for _ = 1, 4 do
        local dgram, from = udp:receivefrom()
        if not dgram then
            -- `from` is the reason: "timeout" is the ordinary one, and it is
            -- worth a line, because the difference between *nothing answered*
            -- and *something answered wrongly* is the whole of a support
            -- conversation about a reader that will not push.
            logger.dbg("bookmole: no answer from", host, tostring(from))
            return nil
        end
        local ok, here = pcall(json.decode, dgram)
        if ok and type(here) == "table" and here.v == PROTOCOL_VERSION
            and type(here.tcp_port) == "number" and type(here.mac) == "string" then
            local want = hmac(entry.token,
                string.format("here:%d:%s:%d", PROTOCOL_VERSION, n, here.tcp_port))
            if want == here.mac then
                logger.dbg("bookmole: answered by", tostring(from), "port", here.tcp_port)
                return { host = from, port = here.tcp_port, name = here.name, nonce = n }
            end
            logger.dbg("bookmole: an answer from", tostring(from), "did not prove the token")
        end
    end
    return nil
end

local function probe(entry, host, timeout)
    local socket = require("socket")
    local udp = socket.udp4()
    if not udp then
        return nil
    end
    udp:setoption("broadcast", true)
    udp:setsockname("*", 0)
    udp:settimeout(timeout or 2)
    -- **The hole is for this socket's own port**, read back from the kernel
    -- rather than fixed. Fixed would be simpler and is wrong twice: it would
    -- collide with the responder an open pull window binds to the rendezvous
    -- port, and a rule is worth the narrowest port it can name. The ask is
    -- wrapped rather than inlined so that the hole is closed on *every* exit —
    -- a rule left behind is a permanently open port on somebody's reader.
    local _, myport = udp:getsockname()
    firewall("A", "udp", myport)
    local found = ask(udp, entry, host)
    firewall("D", "udp", myport)
    udp:close()
    return found
end

-- Walk the ladder. First proven answer wins.
--
-- **A `dns` rung is resolved here, and skipping that made it dead code.**
-- `udp:sendto` takes an address and not a name — measured on a Kindle, where
-- `sendto(msg, "desk", …)` answers `nil, "Name or service not known"`
-- while `socket.dns.toip("desk")` answers `192.0.2.63` on the same
-- resolver a second later. So the three hostname rungs built a probe that never
-- left the device: the rung that is *the best one for a roaming laptop*,
-- silently doing nothing since it was written, and looking correct in a ladder
-- test that only ever asserted the rungs' order.
--
-- It is resolved *here* rather than inside `probe` because this is where a
-- rung's kind is known; `probe` stays about the wire and takes an address.
local function discover(entry, addresses)
    local socket = require("socket")
    for _, rung in ipairs(Bookmole.ladder(addresses, entry.name)) do
        local host = rung.host
        if rung.kind == "dns" then
            local ip, err = socket.dns.toip(host)
            if ip then
                logger.dbg("bookmole:", host, "resolves to", ip)
            else
                -- Ordinary: most routers register a DHCP hostname and some do
                -- not, which is exactly why this is a rung and not the design.
                logger.dbg("bookmole: could not resolve", host, tostring(err))
            end
            host = ip
        end
        -- A cached address is tried by a *probe* rather than a bare connect, so
        -- rung 1 proves the token exactly as the others do — otherwise the
        -- cheapest rung would be the only one that trusts an address.
        local found = host and probe(entry, host, rung.kind == "cached" and 1 or 2)
        if found then
            return found
        end
    end
    return nil
end

-- Send one session: open, one frame per sidecar, done.
local function send(entry, found, sidecars)
    local socket = require("socket")
    local json = require("json")
    local sha = require("ffi/sha2")

    local tcp = socket.tcp()
    tcp:settimeout(10)
    local ok = tcp:connect(found.host, found.port)
    if not ok then
        return nil, _("Could not connect.")
    end

    local function line(tbl)
        return tcp:send(json.encode(tbl) .. "\n")
    end
    local function ack()
        local raw = tcp:receive("*l")
        if not raw then
            return nil
        end
        local parsed, got = pcall(json.decode, raw)
        return parsed and got or nil
    end

    line({
        v = PROTOCOL_VERSION,
        device_id = entry.device_id,
        nonce = found.nonce,
        mac = hmac(entry.token, string.format("open:%d:%s", PROTOCOL_VERSION, found.nonce)),
    })
    local reply = ack()
    if not reply or not reply.ok then
        tcp:close()
        return nil, (reply and reply.error) or _("The computer refused.")
    end

    local sent, failed = 0, 0
    for _, sc in ipairs(sidecars) do
        local body = sc.body
        -- `sha256` returns hex, so it goes into the challenge as-is — the same
        -- string the far side hashes the bytes into.
        local digest = sha.sha256(body)
        line({
            kind = "entry",
            name = sc.name,
            len = #body,
            sha256 = digest,
            mac = hmac(entry.token,
                string.format("body:%d:%s:%s", PROTOCOL_VERSION, found.nonce, digest)),
        })
        tcp:send(body)
        local said = ack()
        if said and said.ok then
            sent = sent + 1
        else
            -- One unreadable sidecar does not end a session carrying good
            -- ones; the far side says so and keeps reading.
            failed = failed + 1
        end
    end
    line({ kind = "done" })
    ack()
    tcp:close()
    return { sent = sent, failed = failed, host = found.host, port = found.port }
end

-- Every sidecar this device holds, as name-and-body pairs.
--
-- The payload is **the sidecar bytes the reader already has**, not a bespoke
-- delta. bookmole parses that format already, the parse is fuzzed, the
-- import is idempotent and the goldens cover it — so wireless is a transport
-- for an import that exists rather than a second import whose first divergence
-- shows up as highlights landing differently depending on which cable they came
-- down.
-- **There is no API that enumerates a device's sidecars**, which is the one
-- thing the design assumed and KOReader does not offer. `DocSettings` answers
-- *where is this document's sidecar* (`findSidecarFile(doc_path)`) and nothing
-- wider; the only whole-device list is `findSidecarFilesInHashLocation()`,
-- which covers one of the three sidecar layouts.
--
-- So the source of books is **`readhistory`**, which is what
-- `plugins/exporter.koplugin/clip.lua:390` uses for the same job — the honest
-- reading of *what this reader has* is what it has opened. The cost is
-- explicit: a book whose history entry has been cleared is not pushed until it
-- is opened again, and pushing over a cable remains the way to get everything.
function Bookmole:collectSidecars()
    local DocSettings = require("docsettings")
    local out = {}
    local seen = {}
    for _, item in ipairs(require("readhistory").hist) do
        local sidecar = item.file and DocSettings:findSidecarFile(item.file)
        if sidecar and not seen[sidecar] then
            seen[sidecar] = true
            local f = io.open(sidecar, "r")
            if f then
                local body = f:read("*a")
                f:close()
                if body and body ~= "" then
                    -- The last two path segments: enough for a person to
                    -- recognise the file in a warning, and not the reader's
                    -- whole directory layout. It is a label, never a handle —
                    -- nothing on the far side opens it.
                    table.insert(out, {
                        name = sidecar:match("([^/]+/[^/]+)$") or sidecar,
                        body = body,
                    })
                end
            end
        end
    end
    return out
end

-- What the two verbs are for, in the reader's own words.
--
-- "Push" and "pull" were the menu's words and they are the protocol's: they
-- name who dials, which is the one thing a person holding a reader does not
-- need to know. What they need is that **both carry the same things the same
-- way** — off this reader, onto the computer — and that the choice is only
-- which device they would rather start from.
function Bookmole:explainText()
    local text = _("bookmole copies this reader's highlights, notes and reading positions to your computer. Nothing on this reader is changed, and nothing is sent unless you choose it here.\n\nThere are two ways to do it, and they carry exactly the same things:\n\n• Send highlights to a computer — start here, on the reader. In bookmole on the computer, open the door first; this reader then finds it and sends.\n\n• Let a computer fetch highlights — start there, on the computer. This reader waits for two minutes; in bookmole, choose Fetch over wifi.\n\nBoth need wifi on, and the reader and the computer on the same network.")
    if #self.pairings == 0 then
        -- Installed, and linked with nobody: `pairing.lua` is absent,
        -- unreadable or empty. Both repairs are things a person can do.
        return text .. "\n\n" .. _("This reader is not linked with a computer yet. Choose Link a computer over wifi, or plug it in and connect it from bookmole.")
    end
    local names = {}
    for _i, entry in ipairs(self.pairings) do
        table.insert(names, Bookmole.computerName(entry))
    end
    -- `%1` is KOReader's own interpolation, applied by `ffi/util.template`.
    -- `string.format` does not understand it.
    return text .. "\n\n" .. T(_("Linked with: %1."), table.concat(names, ", "))
end

-- Push to one computer.
--
-- **Off the UI thread, and off it for the whole of it.** Discovery is up to
-- four rungs at a second or two each and the transfer is however long a library
-- takes, so doing any of it inline freezes e-ink for seconds. `Trapper` wraps
-- the lot in a dismissable subprocess, which is what `httpinspector` and the
-- calibre plugin both do; the subprocess returns a small result and *this* side
-- writes `endpoint.lua`, so nothing but the parent touches our directory.
--
-- **Wifi is asked for, never assumed.** `NetworkMgr:runWhenOnline` prompts or
-- fails according to the user's own setting, which is the only correct place
-- for that decision — a plugin that turned the radio on unasked would be making
-- a power decision on somebody's e-reader.
function Bookmole:push(entry)
    local NetworkMgr = require("ui/network/manager")
    local Trapper = require("ui/trapper")
    NetworkMgr:runWhenOnline(function()
        Trapper:wrap(function()
            local addresses = self:readEndpoints()[entry.device_id]
            local sidecars = self:collectSidecars()
            if #sidecars == 0 then
                UIManager:show(InfoMessage:new{ text = _("There is nothing on this reader to send yet.") })
                return
            end
            local completed, result = Trapper:dismissableRunInSubprocess(function()
                local found = discover(entry, addresses)
                if not found then
                    return { error = "notfound" }
                end
                local sent, err = send(entry, found, sidecars)
                if not sent then
                    return { error = err or "failed" }
                end
                return sent
            end, T(_("Looking for %1…"), Bookmole.computerName(entry)))

            if not completed then
                return -- the user dismissed it; say nothing.
            end
            if type(result) ~= "table" or result.error then
                -- **Fails closed and says which way.** "No bookmole
                -- answered" is a different problem from "it refused", and only
                -- the first one has an action the reader can take.
                local text = T(_("Could not reach bookmole on %1.\n\nIs the door open there, and is it on this wifi network? If it is and this keeps happening, add its address under Linked computers."),
                    Bookmole.computerName(entry))
                if result and result.error and result.error ~= "notfound" then
                    text = T(_("bookmole refused: %1"), tostring(result.error))
                end
                UIManager:show(InfoMessage:new{ text = text })
                return
            end
            -- The device learns the current address as a side effect of the
            -- push working, which is the refresh that costs nothing and keeps
            -- rung 1 useful for a laptop that moves.
            self:rememberEndpoint(entry.device_id, result.host, result.port)
            UIManager:show(InfoMessage:new{
                text = T(_("Sent to %1.\n\nBooks with highlights or a reading position: %2"),
                    Bookmole.computerName(entry), tostring(result.sent)),
            })
        end)
    end)
end

-- ---- the reader's window: pull --------------------------------------------
--
-- The mirror of the desktop's door. The user opens it deliberately, it closes
-- itself, and while it is shut there is nothing on the network to find.
--
-- **`SimpleTCPServer` has no run loop.** It exposes `start`, `stop`, `send` and
-- a `waitEvent` that does exactly one non-blocking `accept`; `UIManager` polls
-- it because `insertZMQ` puts it in the list the main loop drains each cycle,
-- which is how `httpinspector` runs the same class. That is also why this
-- window must be short and must close itself: once a client connects,
-- `waitEvent` reads header lines **in line** with a 100 ms socket timeout and
-- then holds 500 ms for the response, so a stalled peer stalls the UI thread
-- for up to six tenths of a second per connection. Nothing here may be left
-- open "just in case".
--
-- The UDP responder is polled from the same place, for the same reason the
-- desktop's dies with its TCP listener: an announcer that outlives the thing it
-- announces sends a computer to a closed port.
local WINDOW_SECONDS = 120

-- A `waitEvent`-shaped object `UIManager` can poll, wrapping the rendezvous
-- socket. It answers a `HELLO` from a computer we are paired with and is silent
-- to everything else — a probe from a stranger learns nothing, which is what
-- makes "there is nothing to find" true rather than merely intended.
local function makeResponder(entries, tcp_port)
    local socket = require("socket")
    local json = require("json")
    local udp = socket.udp4()
    if not udp then
        return nil
    end
    udp:setoption("broadcast", true)
    if not udp:setsockname("*", RENDEZVOUS_PORT) then
        -- Another program holds the port. Fail closed and silently: the window
        -- simply cannot be found, which is a state the desktop already words.
        udp:close()
        return nil
    end
    -- Zero, not a small number: this is polled from the UI loop, and any
    -- blocking read at all is a stutter on every frame.
    udp:settimeout(0)
    return {
        socket = udp,
        stop = function(self) self.socket:close() end,
        waitEvent = function(self)
            local dgram, host, port = self.socket:receivefrom()
            if not dgram then
                return
            end
            local ok, hello = pcall(json.decode, dgram)
            if not ok or type(hello) ~= "table" or hello.v ~= PROTOCOL_VERSION then
                return
            end
            local entry = entries[hello.device_id]
            if not entry or type(hello.nonce) ~= "string" then
                return
            end
            self.socket:sendto(json.encode({
                v = PROTOCOL_VERSION,
                name = entry.reader_name,
                tcp_port = tcp_port,
                mac = hmac(entry.token,
                    string.format("here:%d:%s:%d", PROTOCOL_VERSION, hello.nonce, tcp_port)),
            }), host, port)
        end,
    }
end

function Bookmole:windowOpen()
    return self.window_server ~= nil
end

-- Serve one pulled session. The desktop opened it, so it sends `OPEN` and we
-- send the entries — the same frames as a push with the dial reversed, which is
-- why nothing about the payload or its MACs changes.
function Bookmole:onPullRequest(data, client)
    local json = require("json")
    local sha = require("ffi/sha2")
    local server = self.window_server

    local function line(tbl)
        client:send(json.encode(tbl) .. "\n")
    end

    local open = nil
    local ok, parsed = pcall(json.decode, (data:gsub("\r\n.*$", "")))
    if ok then open = parsed end
    if type(open) ~= "table" or open.v ~= PROTOCOL_VERSION then
        line({ ok = false, error = "bad request" })
        return server:send("", client)
    end
    local entry = self.pairings_by_id[open.device_id]
    if not entry
        or type(open.nonce) ~= "string"
        or hmac(entry.token, string.format("open:%d:%s", PROTOCOL_VERSION, open.nonce)) ~= open.mac then
        -- No detail. Which byte differed is exactly what an attacker wants.
        line({ ok = false, error = "refused" })
        return server:send("", client)
    end
    line({ ok = true })

    for _, sc in ipairs(self:collectSidecars()) do
        local digest = sha.sha256(sc.body)
        line({
            kind = "entry",
            name = sc.name,
            len = #sc.body,
            sha256 = digest,
            mac = hmac(entry.token,
                string.format("body:%d:%s:%s", PROTOCOL_VERSION, open.nonce, digest)),
        })
        client:send(sc.body)
        -- The far side acks each entry. We do not branch on it: one sidecar it
        -- could not parse is its business and must not stop the rest, which is
        -- the same rule the desktop applies in the other direction.
        client:receive("*l")
    end
    line({ kind = "done" })
    client:receive("*l")
    -- A completed session closes the window, exactly as a completed push closes
    -- the desktop's door: the user asked for one transfer, not for a service.
    UIManager:nextTick(function() self:closeWindow("done") end)
    return server:send("", client)
end

function Bookmole:openWindow()
    if self:windowOpen() then
        return true
    end
    self.pairings_by_id = {}
    local reader = Bookmole.readerName()
    for _i, entry in ipairs(self.pairings) do
        entry.reader_name = reader
        self.pairings_by_id[entry.device_id] = entry
    end
    -- One socket can hold the rendezvous port, and a code on screen is using it.
    self:closePairing("window")

    local ServerClass = require("ui/message/simpletcpserver")
    local server = ServerClass:new{
        host = "*",
        -- Port 0 asks the kernel for a free one, which is announced in every
        -- `HERE`. Nothing on this side is well-known except the UDP port.
        port = 0,
        receiveCallback = function(data, client) return self:onPullRequest(data, client) end,
    }
    local started, err = server:start()
    if not started then
        UIManager:show(InfoMessage:new{ text = T(_("Could not open the window: %1"), tostring(err)) })
        return false
    end
    -- `_host`, not `_`: that is gettext, and the failure message a few lines
    -- down calls it. Shadowed, "something else is using the port" became
    -- *attempt to call a string value* on the one path that needed to speak.
    local _host, port = server.server:getsockname()
    port = tonumber(port)

    local responder = makeResponder(self.pairings_by_id, port)
    if not responder then
        server:stop()
        UIManager:show(InfoMessage:new{
            text = _("Could not open the window: something else is using bookmole's port."),
        })
        return false
    end

    -- **Both halves of the window need a hole, and for different reasons.** The
    -- desktop's `HELLO` is unsolicited, so no conntrack entry can exist for it
    -- and the responder never hears a thing; the dial that follows is an
    -- inbound TCP connection, which is `httpinspector`'s own case exactly. Push
    -- needed one rule for a moment; a window needs two for as long as it is
    -- open, and `closeWindow` is what takes them away again.
    firewall("A", "udp", RENDEZVOUS_PORT)
    firewall("A", "tcp", port)
    self.window_port = port

    self.window_server = server
    self.window_zmq = UIManager:insertZMQ(server)
    self.window_responder = responder
    self.window_responder_zmq = UIManager:insertZMQ(responder)
    -- The window closes itself. `scheduleIn` rather than a deadline checked on
    -- each poll, so a device that goes to sleep and wakes has already been
    -- closed by the standby hook rather than reopening on a stale clock.
    self.window_timer = function() self:closeWindow("timeout") end
    UIManager:scheduleIn(WINDOW_SECONDS, self.window_timer)
    return true
end

-- Close it. Idempotent, because four separate hooks call it and a device can
-- reach two of them in a row.
function Bookmole:closeWindow(why)
    if not self:windowOpen() then
        return
    end
    logger.dbg("bookmole: closing the window,", why)
    if self.window_timer then
        UIManager:unschedule(self.window_timer)
        self.window_timer = nil
    end
    -- The responder goes first: an announcer that outlives its listener sends a
    -- computer to a closed port.
    if self.window_responder then
        UIManager:removeZMQ(self.window_responder_zmq)
        self.window_responder:stop()
        self.window_responder, self.window_responder_zmq = nil, nil
    end
    UIManager:removeZMQ(self.window_zmq)
    self.window_server:stop()
    self.window_server, self.window_zmq = nil, nil
    -- The holes go last, after the sockets they were for are gone: the reverse
    -- of the order they were opened in, so there is no instant where a rule
    -- stands with nothing behind it. Four hooks reach this function and a device
    -- can hit two in a row, which is why the port is cleared as it is used.
    if self.window_port then
        firewall("D", "tcp", self.window_port)
        self.window_port = nil
    end
    firewall("D", "udp", RENDEZVOUS_PORT)
end

-- ---- linking a computer over wifi: the device half ---------------------------

-- How long a code is good for. Long enough to walk to the computer and type
-- sixteen characters; short enough that a code left on screen is not a
-- standing invitation.
local PAIRING_SECONDS = 120
-- After a link, how long the reader keeps repeating its answer. The computer
-- re-sends its request until it hears one, and an answer lost on the air must
-- not leave this reader linked to a computer that thinks it is not.
local PAIRING_GRACE_SECONDS = 10

-- What this reader calls itself. The model, because it is the one name the
-- device knows; the person can rename it on the computer.
function Bookmole.readerName()
    local ok, Device = pcall(require, "device")
    if ok and type(Device) == "table" and type(Device.model) == "string" and Device.model ~= "" then
        return Device.model
    end
    return "reader"
end

-- `n` bytes from the kernel, or nil. **There is no fallback**: a code made
-- from `math.random` is a code somebody can guess from the clock, and a link
-- that cannot be made safely is better not made.
local function randomBytes(n)
    local f = io.open("/dev/urandom", "rb")
    if not f then
        return nil
    end
    local bytes = f:read(n)
    f:close()
    if type(bytes) ~= "string" or #bytes ~= n then
        return nil
    end
    return bytes
end

-- This reader's own address on the network, for the person to read out when
-- the computer cannot find it by itself. A UDP socket that connects and sends
-- nothing: `setpeername` only picks a route. The address is TEST-NET-1, which
-- exists to be unroutable — the same trick, and the same literal, as the
-- installer's `this_lan_address`.
local function ownAddress()
    local ok, socket = pcall(require, "socket")
    if not ok then
        return nil
    end
    local udp = socket.udp4()
    if not udp then
        return nil
    end
    local ip
    if udp:setpeername("192.0.2.1", 9) then
        ip = udp:getsockname()
    end
    udp:close()
    if type(ip) == "string" and ip ~= "0.0.0.0" and not ip:match("^127%.") then
        return ip
    end
    return nil
end

-- The version in `_meta.lua`, which is the installer's only way to tell two
-- plugins apart and therefore the number a computer should record.
function Bookmole:pluginVersion()
    local ok, meta = pcall(dofile, self.path .. "/_meta.lua")
    if ok and type(meta) == "table" and type(meta.version) == "number" then
        return meta.version
    end
    return 0
end

function Bookmole:pairingOpen()
    return self.pairing ~= nil
end

-- A computer proved it holds the code. Store it, *then* answer — a reply sent
-- before the file landed is a computer that believes in a link this reader
-- forgets on restart.
function Bookmole:finishPairing(entry, host)
    if not self:savePairings(Bookmole.upsertPairing(self:readPairings(), entry)) then
        return false
    end
    -- Where the request came from is where the computer is, right now. A hint
    -- like every other address here: the next push proves it or replaces it.
    self:rememberEndpoint(entry.device_id, host, 0)
    return true
end

function Bookmole:openPairing()
    local NetworkMgr = require("ui/network/manager")
    -- Wifi must already be up, for the pull window's reason: a prompt for the
    -- radio and then a code nobody is looking at is the wrong order.
    if not NetworkMgr:isOnline() then
        UIManager:show(InfoMessage:new{ text = _("Turn wifi on first, then link a computer.") })
        return
    end
    self:closeWindow("pairing")
    self:closePairing("again")

    local bytes = randomBytes(48)
    local code = bytes and Bookmole.codeFromBytes(bytes)
    if not code then
        UIManager:show(InfoMessage:new{
            text = _("This reader cannot make a safe code, so it cannot be linked over wifi. Plug it in and connect it from bookmole instead."),
        })
        return
    end

    local socket = require("socket")
    local json = require("json")
    local udp = socket.udp4()
    if udp then
        udp:setoption("broadcast", true)
        if not udp:setsockname("*", RENDEZVOUS_PORT) then
            udp:close()
            udp = nil
        end
    end
    if not udp then
        UIManager:show(InfoMessage:new{
            text = _("Could not start: something else is using bookmole's port."),
        })
        return
    end
    -- Zero: polled from the UI loop, exactly like the pull window's responder.
    udp:settimeout(0)

    local state = {
        code = code,
        pairings = self.pairings,
        plugin = self:pluginVersion(),
        reader = Bookmole.readerName(),
    }
    -- The nonce and the id a new computer would get, minted from the bytes
    -- that did not go into the code.
    local function mint()
        return Bookmole.hexFromBytes(bytes:sub(17, 32)), Bookmole.hexFromBytes(bytes:sub(33, 48))
    end

    local responder = {
        socket = udp,
        stop = function(this) this.socket:close() end,
        waitEvent = function(this)
            local dgram, host, port = this.socket:receivefrom()
            if not dgram then
                return
            end
            local ok, msg = pcall(json.decode, dgram)
            if not ok then
                return
            end
            local reply, entry = Bookmole.answerPair(state, msg, hmac, mint)
            if not reply then
                return
            end
            if entry and not self:finishPairing(entry, host) then
                -- Not stored, so not linked: say nothing to the computer, and
                -- say why here. The code is spent either way.
                self:closePairing("unwritable")
                UIManager:show(InfoMessage:new{
                    text = _("Could not save the link on this reader. Is its storage full or read-only?"),
                })
                return
            end
            this.socket:sendto(json.encode(reply), host, port)
            if entry then
                self:pairingLinked(entry)
            end
        end,
    }

    -- The computer's request is unsolicited, so on a Kindle nothing lets it in
    -- until this does — the pull window's first rule, for the same reason.
    firewall("A", "udp", RENDEZVOUS_PORT)
    self.pairing = {
        responder = responder,
        zmq = UIManager:insertZMQ(responder),
        timer = function() self:closePairing("timeout") end,
    }
    UIManager:scheduleIn(PAIRING_SECONDS, self.pairing.timer)

    local address = ownAddress()
    local text = T(_("To link a computer:\n\n1. In bookmole on the computer, open Devices and choose Link a reader over wifi.\n2. Type this code there:\n\n%1\n\nThe code works once, for two minutes. It is never sent over the network."),
        Bookmole.formatCode(code))
    if address then
        text = text .. "\n\n" .. T(_("If the computer cannot find this reader, give it this address: %1"), address)
    end
    self.pairing.dialog = InfoMessage:new{
        text = text,
        -- Putting the code away withdraws it. A code that outlives the screen
        -- it was shown on is one nobody is watching.
        dismiss_callback = function()
            if self.pairing and not self.pairing.linked then
                self:closePairing("dismissed")
            end
        end,
    }
    UIManager:show(self.pairing.dialog)
end

-- The link is made. Swap the code for the news, and keep answering for a few
-- seconds in case the computer did not hear the first time.
function Bookmole:pairingLinked(entry)
    local pairing = self.pairing
    if not pairing then
        return
    end
    pairing.linked = true
    if pairing.dialog then
        UIManager:close(pairing.dialog)
        pairing.dialog = nil
    end
    UIManager:unschedule(pairing.timer)
    UIManager:scheduleIn(PAIRING_GRACE_SECONDS, pairing.timer)
    UIManager:show(InfoMessage:new{
        text = T(_("Linked with %1.\n\nIt is in the bookmole menu now: Send highlights to %1."),
            Bookmole.computerName(entry)),
    })
end

-- Withdraw the code. Idempotent, for `closeWindow`'s reason.
function Bookmole:closePairing(why)
    local pairing = self.pairing
    if not pairing then
        return
    end
    self.pairing = nil
    logger.dbg("bookmole: withdrawing the pairing code,", why)
    UIManager:unschedule(pairing.timer)
    UIManager:removeZMQ(pairing.zmq)
    pairing.responder:stop()
    firewall("D", "udp", RENDEZVOUS_PORT)
    if pairing.dialog then
        UIManager:close(pairing.dialog)
    end
end

-- **Four hooks, not one.** `onEnterStandby` alone looks sufficient;
-- `httpinspector` handles all four and it is right — a device suspends, it also
-- exits, and a widget closing under an open socket leaks it. A window left open
-- across a suspend is a listening port on a device in somebody's bag, and a
-- pairing code left open is the same port with a secret behind it.
function Bookmole:closeEverything(why)
    self:closeWindow(why)
    self:closePairing(why)
end
function Bookmole:onEnterStandby() self:closeEverything("standby") end
function Bookmole:onSuspend() self:closeEverything("suspend") end
function Bookmole:onExit() self:closeEverything("exit") end
function Bookmole:onCloseWidget() self:closeEverything("close") end

-- ---- the menu ---------------------------------------------------------------

-- One address of one computer, as a line: where, and how this reader knows.
function Bookmole.addressLabel(address)
    if address.typed then
        return T(_("%1 — typed in"), address.host)
    end
    if (address.seen_at or 0) > 0 then
        return T(_("%1 — last used %2"), address.host, os.date("%d %b %Y", address.seen_at))
    end
    return address.host
end

-- Ask for an address and keep it.
function Bookmole:askForAddress(entry)
    local InputDialog = require("ui/widget/inputdialog")
    local dialog
    dialog = InputDialog:new{
        title = T(_("An address for %1"), Bookmole.computerName(entry)),
        description = _("The computer's address on this network — like 192.0.2.20 — or its name. bookmole shows it on the Devices page. This reader tries it first from now on."),
        input = "",
        buttons = {{
            {
                text = _("Cancel"),
                id = "close",
                callback = function() UIManager:close(dialog) end,
            },
            {
                text = _("Keep it"),
                is_enter_default = true,
                callback = function()
                    local host = Bookmole.validAddress(dialog:getInputText())
                    if not host then
                        UIManager:show(InfoMessage:new{
                            text = _("That does not look like an address. Use numbers and dots, like 192.0.2.20, or a name like desk.lan."),
                        })
                        return
                    end
                    UIManager:close(dialog)
                    local endpoints = self:readEndpoints()
                    endpoints[entry.device_id] =
                        Bookmole.addTypedAddress(endpoints[entry.device_id], host)
                    if not self:saveEndpoints(endpoints) then
                        UIManager:show(InfoMessage:new{ text = _("Could not save that on this reader.") })
                    end
                end,
            },
        }},
    }
    UIManager:show(dialog)
    dialog:onShowKeyboard()
end

-- One computer: what this reader knows about it, and the way out.
function Bookmole:computerMenu(entry)
    local ConfirmBox = require("ui/widget/confirmbox")
    local name = Bookmole.computerName(entry)
    local items = {}
    for _i, address in ipairs(self:readEndpoints()[entry.device_id] or {}) do
        table.insert(items, {
            text = Bookmole.addressLabel(address),
            help_text = _("An address this reader tries when it looks for this computer. Tap to forget it."),
            callback = function()
                UIManager:show(ConfirmBox:new{
                    text = T(_("Forget %1 as an address for %2?\n\nThis reader can still find the computer by itself when they are on the same network."),
                        address.host, name),
                    ok_text = _("Forget it"),
                    ok_callback = function()
                        local endpoints = self:readEndpoints()
                        endpoints[entry.device_id] =
                            Bookmole.removeAddress(endpoints[entry.device_id], address.host)
                        self:saveEndpoints(endpoints)
                    end,
                })
            end,
        })
    end
    table.insert(items, {
        text = _("Add an address"),
        help_text = _("For a network where this reader cannot find the computer by itself. Most home networks never need one."),
        callback = function() self:askForAddress(entry) end,
    })
    table.insert(items, {
        text = T(_("Unlink %1"), name),
        separator = true,
        callback = function()
            UIManager:show(ConfirmBox:new{
                text = T(_("Unlink %1?\n\nThis reader will stop sending to it and stop answering it. Nothing already on the computer is removed, and the computer still lists this reader until you forget it there."),
                    name),
                ok_text = _("Unlink"),
                ok_callback = function()
                    if not self:savePairings(Bookmole.removePairing(self:readPairings(), entry.device_id)) then
                        UIManager:show(InfoMessage:new{ text = _("Could not save that on this reader.") })
                        return
                    end
                    local endpoints = self:readEndpoints()
                    endpoints[entry.device_id] = nil
                    self:saveEndpoints(endpoints)
                    -- An open window was answering for this computer too.
                    self:closeWindow("unlinked")
                end,
            })
        end,
    })
    return items
end

-- The bookmole menu, built each time it is opened — a computer linked a
-- minute ago is in it without a restart.
function Bookmole:buildMenu()
    local sub = {}
    -- One entry per linked computer, named.
    --
    -- **The index is named, and must stay named.** `for _, entry` is the idiom
    -- in the oldest parts of this file and is wrong on exactly this loop: `_`
    -- is gettext, bound at the top of the module, and the loop body is the one
    -- place that calls it. Shadowing it makes the next line `(a number)(...)`,
    -- which throws while KOReader is *building* the menu — so the failure is
    -- not a broken entry, it is **no bookmole entry in Tools at all**, on a
    -- reader that is paired and looks fine otherwise. Assigning `_ = i` to
    -- silence an unused variable would *also* be the bug, since `_` here is the
    -- module's upvalue and the assignment would replace gettext with a number
    -- for the rest of the process.
    for _idx, entry in ipairs(self.pairings) do
        table.insert(sub, {
            text = T(_("Send highlights to %1"), Bookmole.computerName(entry)),
            help_text = _("Start here, on the reader. Copies this reader's highlights, notes and reading positions to that computer now. Open the door in bookmole there first."),
            keep_menu_open = true,
            callback = function()
                self:push(entry)
            end,
        })
    end
    -- The other direction: let the computer come and get it. One entry, not one
    -- per computer — the window is open to every linked computer at once, and
    -- which of them dials is the desktop's choice rather than the reader's.
    if #self.pairings > 0 then
        table.insert(sub, {
            text_func = function()
                return self:windowOpen() and _("Stop waiting for a computer")
                    or _("Let a computer fetch highlights")
            end,
            help_text = _("Start there, on the computer. This reader waits for two minutes; in bookmole, choose Fetch over wifi. It carries the same things as sending, and nothing on this reader changes."),
            keep_menu_open = true,
            callback = function()
                if self:windowOpen() then
                    self:closeWindow("menu")
                    UIManager:show(InfoMessage:new{ text = _("This reader has stopped waiting.") })
                    return
                end
                -- Wifi must already be up. `runWhenOnline` would prompt for the
                -- radio and then open a window the user walks away from, which
                -- is the opposite of a door you hold open on purpose.
                local NetworkMgr = require("ui/network/manager")
                if not NetworkMgr:isOnline() then
                    UIManager:show(InfoMessage:new{
                        text = _("Turn wifi on first, then try again."),
                    })
                    return
                end
                if self:openWindow() then
                    UIManager:show(InfoMessage:new{
                        text = _("This reader is waiting for two minutes. In bookmole on your computer, choose Fetch over wifi.\n\nIt stops by itself, and when the reader sleeps."),
                    })
                end
            end,
        })
        table.insert(sub, {
            text = _("Linked computers"),
            separator = true,
            sub_item_table_func = function()
                local computers = {}
                for _idx, entry in ipairs(self.pairings) do
                    table.insert(computers, {
                        text = Bookmole.computerName(entry),
                        sub_item_table_func = function() return self:computerMenu(entry) end,
                    })
                end
                return computers
            end,
        })
    end
    table.insert(sub, {
        text = _("Link a computer over wifi"),
        help_text = _("Shows a code to type into bookmole on a computer. No cable needed, and a reader can be linked with several computers."),
        callback = function() self:openPairing() end,
    })
    table.insert(sub, {
        text = _("How sending and fetching work"),
        keep_menu_open = true,
        callback = function()
            UIManager:show(InfoMessage:new{ text = self:explainText() })
        end,
    })
    return sub
end

function Bookmole:addToMainMenu(menu_items)
    -- `sorting_hint = "tools"` must name a menu id that exists, or
    -- `menusorter.lua` indexes a nil and KOReader crashes on startup. "tools"
    -- resolves in both the file-manager and the reader order tables.
    menu_items.bookmole = {
        text = _("bookmole"),
        sorting_hint = "tools",
        sub_item_table_func = function() return self:buildMenu() end,
    }
end

return Bookmole
