-- A settings store for frontends.
--
-- Display choices — which panels a view shows, how a chart is grouped and what
-- it compares — are choices a reader makes once and expects to find again: on
-- the next launch and in the next frontend. A frontend's own storage (a
-- webview's `localStorage`, say) is neither:
-- it is per-frontend and it is wiped with the webview's data. So they live
-- here, beside the library they describe.
--
-- ## The engine stores, it does not interpret
--
-- `key` is a frontend's name for a choice (a dotted name such as
-- `view.panels`) and `value` is that choice as **JSON text**. The engine
-- validates exactly two things — the key's alphabet (`[a-z0-9._-]{1,64}`) and
-- that `value` parses as JSON — and reads neither. A setting the engine itself
-- branched on would be a configuration surface, and that belongs in the
-- options a library is opened with (`Api::open`), not in a blob it stores.
--
-- ## What does not go here
--
-- **No secrets.** The Google Books key keeps its own path
-- (`Engine::set_google_api_key`), which knows how to keep a live copy and never
-- logs it; a JSON blob in a table every frontend can list is the opposite of
-- that.
--
-- `updated_at` is unix seconds, stamped by every write. No back-fill: the table
-- is new and holds nothing until a frontend writes to it.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
