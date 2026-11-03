# Public preview links

Joseph, 2026-11-02: "a feature where you could set a portal link to be public, either for one
hour, one day, one week or indefinitely."

## What boat does (seen live, 2026-11-02)

A probe VM (`small`), `POST /sandboxes/{id}/host` on a `python3 -m http.server` on port 3000,
fetched with no cookies and no redirects followed:

| Step | boat answers | The URL with no token | The old token's URL |
| - | - | - | - |
| `{"public": false}` | `https://<sub>-3000.on.boat.dev?_token=…`, `access: private`; asked again, the same token | 403 "This preview requires an access token" | — |
| `{"public": true}` | the same origin with no token, `access: public`, `isProtected: false` | 200 at once | 200 |
| `{"public": false}` again | a **new** token | 403 at once (and 5 s later) | 403: the old private link is dead too |
| in-VM `host hide 3000` (through exec) | "Port 3000 is now private", the route unregistered | 302 to boat's fallback page | 302 |
| hosted private after the hide | a new token again | 403 | 403 |
| VM stopped, `host` either way | `400 machine_not_running` | hangs, then boat's fallback page | — |
| VM resumed, before hosting again | — | 302 to boat's fallback for every URL | — |
| resumed, hosted public | the same URL, public again | 200 | — |

- **Public is the same URL without the token.** Making it private again mints a new token, so
  every private link Flotilla held for that port stops working and has to be replaced.
- **There's no unhost call in the API.** `host hide <port>` exists only as the in-VM `host`
  command (reachable through `POST /commands`). Flotilla doesn't need it: `public: false` closes
  the link at once.
- **Public also opens the port in the VM's firewall** (`ufw`: `3000/tcp ALLOW Anywhere`, IPv4
  and IPv6), so the raw TCP port is reachable on the VM's IP too. Private takes the rule away.
- **Nothing reads a port's access back.** The sandbox has no hosted-ports field; only the in-VM
  `host list` shows them. Flotilla keeps what it made public.
- **A stopped VM serves nothing and isn't woken by a request.** After a resume the route didn't
  serve until the port was hosted again (boat's docs say the URL stays the same).
- **No cost, a limit of 50 hosted ports per VM.** boat's pricing lists public URLs as included.
- **The agent in the VM can make a port public itself** with boat's in-VM `host <port>
  --public` (its `ASCII_TOKEN` may host its own VM's ports). Flotilla's refusal covers
  Flotilla's API only; see "For Joseph" below.

## What Flotilla does

- **Make public…** (desktop: a service's row in Preview's menu and the port dialog; iPhone: the
  same in the sheet and the port field): "Anyone with the link can open it, without signing
  in." and 1 hour, 1 day, 1 week, Until I stop it. `thread.make_public {thread, port, for}`
  wakes a stopped VM first, as Preview does, and answers the link, which the app copies.
  Making it public again starts its time over.
- **While public:** "Public · 58 min left" or "Public until you stop it", Copy link, Make
  private (`thread.make_private`, which never wakes a VM). The header's Preview reads Public (an
  open lock on the iPhone).
- **The server owns the clock** (`flotilla_core::public`, `flotilla_engine::public`). Each link
  is kept in `kv` as `public_link:<thread>:<port>` (its VM, URL, since, until); no migration. A
  worker looks at least every 5 s, whether or not an app is open, and makes due links private.
  A restart loses nothing: one due while the server was off ends at the first look.
- **Kept before boat is asked.** Make public writes the link first, so it's never public on
  boat and unknown here (an answer lost on the way, a crash). If boat fails, a link that was
  public stays as it was; one that wasn't is put back to private, and if boat can't confirm that
  either, it shows as "Public · ending" and the worker tries again.
- **Hidden only once it's closed.** A link stops showing once boat has made it private, or its
  VM is stopped (Flotilla's state says Stopped, or boat says `machine_not_running`): a VM
  stopping, starting or in error is asked. If boat fails otherwise (Make private, archiving,
  its time up), it stays shown, ending, and is tried again at every look.
- **A rebuilt database carries the links over** (`Store::backup`). The new database is made
  whole beside the old one first, as one file with no WAL (`.rebuilt`), and where the old one
  goes is written down; then the old one is moved aside and the new one put in its place. A
  crash between the two moves is finished by the next open, alone (no other open under way),
  which first moves any of the old one's WAL files left behind to where it went, so SQLite never
  reads them into the new one.
  Opening a database, and anything that hosts a port or ends a link (Make public or private,
  Preview, a service's link, a wake, archiving, the worker), waits for a rebuild under way;
  given a connection opened before one (it has the file moved aside), it opens the database in
  use and works there, going by the thread as that one has it. A link whose thread isn't back
  yet is kept, and ended through boat alone when it's due (a VM that's gone is forgotten).
- **An archived thread keeps nothing public**, however archiving was cut off (a crash, a
  rebuild): archiving first makes its links due now in their own records (which a rebuild
  carries, as it doesn't the archive), then marks it archived; the worker ends them, a wake (for
  `.agents/archive`) doesn't host them public again, and Preview hosts them private. Unarchiving
  doesn't undo that; Make public brings the thread back from the archive, as a message does.
- **A lock per thread around every host call** (Preview, a service's link, Make public/private,
  the worker), so nothing flips a link behind another's back: Preview on a public port hosts it
  public again rather than private, and on one past its time hosts it private. The worker's
  clock never waits for boat: each look hands each due thread's ends to a worker of its own (at
  most 8 under way), passes by a thread whose ends are under way, and looks again within a
  second while any are; so a link that comes due while another's end waits on a slow VM still
  ends on time. Threads boat hasn't been asked about go first, then those asked longest ago, so
  ends that keep failing never take every place from a link that hasn't had its turn. The rebuild gate is per database, so one database's rebuild holds up no other's.
- **Services' links** take the new token when a link goes private again: the newest private URL
  boat gave is kept (`preview_url:<thread>:<port>`) and `services::state` puts it in.
- **A stopped VM: the clock keeps running, and the link stays public.** "1 hour" is an hour of
  the day, which is what someone sharing a link expects, and a stopped VM serves nothing anyway.
  boat can't be told while the VM is stopped, so a link whose time ran out (or that you made
  private, or whose thread you archived) is marked ended, stops showing, and goes private as
  Flotilla wakes the VM, before anything is sent to it. One with time left is hosted public
  again then, so it carries on working. One that ended while its VM was stopped, on a VM woken
  outside Flotilla (boat's CLI), goes private within seconds of Flotilla seeing the VM up.
- **Archiving ends every link** (private now, or as the VM wakes). **Deleting or forgetting**
  a thread makes its links due and keeps them: the worker ends them through boat alone, as it
  does one whose thread a rebuild hasn't brought back, and forgets each once boat has it
  private or says the VM is gone (`thread.forget` leaves the VM up; a deleted VM answers 404).
  Forget is refused while a link of the thread is being changed, and a link isn't made public
  for a thread that isn't in the database. A link on a VM its thread has moved off (a fork
  labelled as it) is ended there the same way, since that VM may still serve it; making the
  same port public on the new VM ends it first (refused until boat confirms).
- **Agents can't**: `Request::agent_refusal` names both requests (any duration), and the MCP
  tool's private link says when the owner made it public.
- **Testing the end:** `FLOTILLA_DEBUG_PUBLIC_SECS=<n>` (10 or more) makes every timed choice
  last that many seconds on that server; it never lengthens one, and Until I stop it is never
  shortened.

## Not done

- **A `public:` default in `.agents/services.yml`.** Flotilla reads the services file from the
  thread's VM, which the agent can edit, so a default there would let an agent make a link
  public: left out until the file is read from the default branch on GitHub.
- **Keeping the VM up while a link is public.** A public link still stops serving when the VM
  stops idle; it works again once the VM wakes.
