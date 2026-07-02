<!--
Copyright (C) 2026 The Argus Capture community

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program. If not, see <https://www.gnu.org/licenses/>.
-->

# Browser remote

## Basic overview

Canon's **Browser Remote** mode is a small web application served directly by the
camera over HTTP. On the tested camera it lives under the `/wpd/` path and is
protected by a separate **Digest Auth** realm named `BrowserRemote`.

The important reverse-engineering takeaway is that Browser Remote is **not**
implemented as a completely separate control stack. The HTML and JavaScript in
`/wpd/` use a mix of:

1. **Browser Remote-specific endpoints** under `/brapi/...`
2. **Canon CCAPI endpoints** under `/ccapi/...`

In practice, Browser Remote appears to be a thin JavaScript UI layered on top
of CCAPI, with a few `/brapi` endpoints for session state, page heartbeats, and
Browser Remote-only properties.

For the smaller implementation-facing summary that tracks what Argus Capture is
actively wiring into the GTK UI, see `doc/CAMERA_API.md`.

## Configuration

The camera connection details come from `argus-capture.conf`, using the same
host, port, username, and password that `argus-capture` already loads.

The sample file is `doc/etc/argus-capture.conf`.

```ini
[ArgusCapture]
workspace = .
storage = workspace_only
camera = Camera1

[Camera1]
host = camera.example.local
port = 443
# username = your-camera-username
# password = your-camera-password
```

For the live probing done for this note, the active config resolved to:

- host from the selected camera section
- port from the selected camera section
- Browser Remote username/password from the same section

## Entry points and auth flow

Observed behavior on the camera:

| Request | Result |
| --- | --- |
| `GET /` | `303 See Other` to `/brapi/login` |
| `GET /brapi/login` | `401 Unauthorized` with Digest realm `BrowserRemote` |
| Authenticated `GET /brapi/login` | `303 See Other` to `/wpd/topmenu.shtml` on success |
| `GET /brapi/logout` | `303 See Other` to `/wpd/logout.shtml`, clears `brsessionid` |

The successful login response sets a session cookie:

```text
Set-Cookie: brsessionid=<id>; Path=/; HttpOnly
```

### Session collisions

Browser Remote is effectively a **single-user web session**.

If another client is already connected with the same login, the camera can
return:

```text
303 See Other
Location:/wpd/already_login.shtml
```

That page displays:

> A user with the same login name is already accessing the camera

In practice, calling `/brapi/logout` before logging in is useful when
reverse-engineering or automating the interface.

## Browser Remote page map

The post-login landing page is:

- `/wpd/topmenu.shtml`

Observed stage pages:

- `/wpd/topmenu.shtml`
- `/wpd/play.shtml`
- `/wpd/shoot.shtml`
- `/wpd/meta.shtml`
- `/wpd/ftp.shtml`

Observed partial templates / support pages:

- `/wpd/appMenu.shtml`
- `/wpd/messages.shtml`
- `/wpd/already_login.shtml`
- `/wpd/logout.shtml`

`topmenu.shtml` renders the high-level app navigation:

- **Playback**
- **Shooting**
- **IPTC info**
- **FTP settings**

`shoot.shtml` renders the actual camera control UI, including:

- live view canvases
- shutter / AF controls
- record button
- drive focus controls
- selectors for TV, AV, exposure compensation, ISO, white balance, metering,
  drive mode, image quality, and related settings

## Front-end structure

Observed JavaScript loaded by the UI:

- `./js/base_script.js`
- `./js/appMenu_script.js`
- `./js/shoot_script.js`
- `./js/cookie_script.js` (inserted dynamically by `appMenu_script.js`)

Observed third-party dependency:

- `./oss/jquery/jquery-3.3.1.min.js`

Notable implementation details from `base_script.js`:

- the UI is touch-first, but supports mouse events too
- all API traffic goes through `ccapiReq(...)`
- requests set:
  - `Pragma: no-cache`
  - `Cache-Control: no-cache`
  - `If-Modified-Since: Sat, 01 Jan 2000 00:00:00 GMT`
  - `Content-type: application/json; charset=utf-8`
- a `403` response causes the UI to redirect to `/wpd/logout.shtml`

## Browser Remote-specific endpoints (`/brapi`)

Observed directly or from the shipped JavaScript:

| Endpoint | Notes |
| --- | --- |
| `/brapi/login` | Digest login entrypoint |
| `/brapi/logout` | Session teardown |
| `/brapi/heartbeat?id=<n>` | Lightweight status heartbeat; page IDs observed in JS |
| `/brapi/property/location` | Browser Remote property endpoint |
| `/brapi/simultaneous_function` | Used to decide whether FTP should be enabled |
| `/brapi/currentproperty` | Queried by `shoot_script.js` for the initial full state |
| `/brapi/shooting/lvscrolldetail` | Browser Remote live-view transport helper |

### Observed heartbeat page IDs

From `appMenu_script.js`:

| Page | Heartbeat ID |
| --- | --- |
| `topmenu` | `0` |
| `play` | `1` |
| `shoot` | `2` |
| `meta` | `3` |
| `ftp` | `4` |
| `play_viewer` | `5` |

### `currentproperty` response shape

Observed live response from `GET /brapi/currentproperty` is a single JSON object
containing the full shooting-page state. It mixes:

- control state objects such as:
  - `moviemode.status`
  - `recbutton.status`
  - `liveview.liveviewsize`
  - `liveview.cameradisplay`
- ordinary settings as `{ value, ability }`, for example:
  - `shootingmode`
  - `av`
  - `iso`
  - `wb`
  - `afoperation`
  - `afmethod`
  - `drive`
  - `metering`
- numeric range settings where `ability` is `{min,max,step}`, for example:
  - `colortemperature`
  - `wbshift.ba`
  - `wbshift.mg`
  - still-image compression controls
- card / recording function settings such as:
  - `recordfunctions_stillimage`
  - `recordfunctions_movie`
  - `cardselection_stillimage`
  - `cardselection_movie`

Observed example:

```json
{
  "moviemode": { "status": "off" },
  "recbutton": { "status": "stop" },
  "recordable": { "recordableshots": 7189, "remainingtime": null },
  "temperature": { "status": "normal" },
  "liveview": { "liveviewsize": "off", "cameradisplay": "on" },
  "shootingmode": {
    "value": "av",
    "ability": ["fv", "p", "av", "m", "tv", "bulb", "c1"]
  },
  "av": {
    "value": "f3.2",
    "ability": ["f2.0", "f2.2", "f2.5", "f2.8", "f3.2", "..."]
  },
  "iso": {
    "value": "1600",
    "ability": ["auto", "200", "250", "320", "..."]
  },
  "colortemperature": {
    "value": 5200,
    "ability": { "min": 2500, "max": 10000, "step": 100 }
  }
}
```

This resolves the earlier uncertainty: `/brapi/currentproperty` is the Browser
Remote UI's **full initial state document**, not just a small status ping.

### `heartbeat` semantics

Observed live responses from:

- `GET /brapi/heartbeat?id=0`
- `GET /brapi/heartbeat?id=2`

were:

```json
{"moviemode":"off","hfflickerstatus":"idle"}
```

So `/brapi/heartbeat` appears to be a **small delta/status probe**, not a full
state snapshot. The JavaScript uses it for:

- movie mode changes
- HF flicker status
- FTP warning state (when present)

The page ID selects which Browser Remote stage is considered active, but the
observed response format is simple JSON rather than HTML or an opaque token.

## CCAPI endpoints used by Browser Remote

This is the key connection to **CCAPI**: the Browser Remote UI is built on top
of ordinary camera-control endpoints under `/ccapi/...`.

### From `appMenu_script.js`

- `/ccapi/ver100/functions/registeredname/nickname`
- `/ccapi/ver100/shooting/control/moviemode`
- `/ccapi/ver100/devicestatus/battery`
- `/ccapi/ver100/devicestatus/temperature`

### From `shoot_script.js`

Representative examples:

- `/ccapi/ver100/shooting/settings/shootingmode`
- `/ccapi/ver100/shooting/settings/tv`
- `/ccapi/ver100/shooting/settings/av`
- `/ccapi/ver100/shooting/settings/exposure`
- `/ccapi/ver100/shooting/settings/iso`
- `/ccapi/ver100/shooting/settings/picturestyle`
- `/ccapi/ver100/shooting/settings/wb`
- `/ccapi/ver100/shooting/settings/afoperation`
- `/ccapi/ver100/shooting/settings/afmethod`
- `/ccapi/ver100/shooting/settings/metering`
- `/ccapi/ver100/shooting/settings/drive`
- `/ccapi/ver100/shooting/settings/moviequality`
- `/ccapi/ver100/shooting/settings/stillimageaspectratio`
- `/ccapi/ver100/shooting/information/recordable`
- `/ccapi/ver100/devicestatus/battery`
- `/ccapi/ver100/devicestatus/temperature`
- `/ccapi/ver100/event/polling?continue=on`
- `/ccapi/ver100/shooting/control/af`
- `/ccapi/ver100/shooting/control/drivefocus`
- `/ccapi/ver100/shooting/control/moviemode`
- `/ccapi/ver100/shooting/control/recbutton`
- `/ccapi/ver100/shooting/control/shutterbutton/manual`
- `/ccapi/ver100/shooting/liveview`
- `/ccapi/ver100/shooting/liveview/afframeposition`
- `/ccapi/ver110/shooting/settings/hfflickertv`
- `/ccapi/ver110/shooting/settings/hdr`
- `/ccapi/ver110/shooting/settings/shuttermode`
- `/ccapi/ver110/shooting/settings/highframerate`
- `/ccapi/ver110/shooting/settings/moviecropping`
- `/ccapi/ver110/customfunctions/exposureincrements/av`
- `/ccapi/ver110/customfunctions/exposureincrements/tv`
- `/ccapi/ver110/customfunctions/exposureincrements/exposure`
- `/ccapi/ver110/customfunctions/isoincrements`
- `/ccapi/ver110/shooting/control/hfflickerdetection`
- `/ccapi/ver110/shooting/control/flickerdetection`

### Practical interpretation

For Argus Capture, this means Browser Remote is best understood as:

- **web auth/session layer**: `/brapi/...`
- **camera control layer**: `/ccapi/...`
- **presentation layer**: `/wpd/*.shtml` + `*.js`

If Argus Capture wants to automate Browser Remote features, it should prefer
talking to **CCAPI directly** where possible, and only fall back to `/brapi`
when the browser UI depends on Browser Remote-only state.

## Request model used by the Browser Remote JavaScript

The browser UI centralizes camera I/O through a helper named `exec_api(...)` in
`base_script.js` / `shoot_script.js`.

Observed request behavior:

- method is one of `GET`, `POST`, `PUT`, or `DELETE`
- JSON bodies are serialized with `JSON.stringify(...)`
- requests always set:
  - `Pragma: no-cache`
  - `Cache-Control: no-cache`
  - `If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT`
- when a JSON body is present, requests also set:
  - `Content-type: application/json; charset=utf-8`
- a `403` causes the UI to redirect to `logout.shtml`

### Browser Remote session model

Observed behavior suggests:

1. Use Digest auth only for `GET /brapi/login`
2. After login, use the returned `brsessionid` cookie for `/wpd`, `/brapi`, and
   `/ccapi` requests made by the Browser Remote web UI
3. Do not assume multiple parallel Browser Remote sessions will work

For future Argus Capture work, the safest pattern is:

1. `GET /brapi/logout`
2. login via `GET /brapi/login` with Digest auth
3. capture the returned `brsessionid`
4. make follow-up Browser Remote requests with that cookie

## API reference

The sections below are based on the actual Browser Remote HTML/JS served by the
camera and are intended to be specific enough to drive future automation work.

### Login and session endpoints

| Method | Path | Request body | Response / notes |
| --- | --- | --- | --- |
| `GET` | `/brapi/login` | none | Digest auth challenge with realm `BrowserRemote` |
| `GET` | `/brapi/login` | Digest auth | `303` to `/wpd/topmenu.shtml` on success |
| `GET` | `/brapi/logout` | none | `303` to `/wpd/logout.shtml`, expires `brsessionid` |
| `GET` | `/wpd/already_login.shtml` | none | warning page shown when another session owns the login |

### Browser Remote page and template endpoints

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/wpd/topmenu.shtml` | top-level Browser Remote landing page |
| `GET` | `/wpd/play.shtml` | playback stage |
| `GET` | `/wpd/shoot.shtml` | shooting stage |
| `GET` | `/wpd/meta.shtml` | IPTC metadata stage |
| `GET` | `/wpd/ftp.shtml` | FTP settings stage |
| `GET` | `/wpd/appMenu.shtml` | shared app menu partial |
| `GET` | `/wpd/messages.shtml` | UI message table as embedded JSON |

### Browser Remote helper endpoints

| Method | Path | Request body / query | Notes |
| --- | --- | --- | --- |
| `GET` | `/brapi/heartbeat?id=<n>` | query only | page heartbeat, returns a small JSON status delta |
| `POST` | `/brapi/property/location` | `{"hostname":"...","host":"..."}` | sent from `appMenu_script.js` after load |
| `GET` | `/brapi/simultaneous_function` | none | used to enable/disable FTP stage |
| `GET` | `/brapi/currentproperty` | none | initial shooting-page state snapshot |
| `GET` | `/brapi/shooting/lvscrolldetail?liveviewsize=<size>` | query only | starts Browser Remote live-view binary stream |
| `DELETE` | `/brapi/shooting/lvscrolldetail?liveviewsize=off` | none | stops Browser Remote live-view binary stream |

### CCAPI reads used by Browser Remote

These are directly embedded in `SHOOTING_SETTINGS` and `apilist_appMenu`.

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/ccapi/ver100/functions/registeredname/nickname` | camera nickname for top bar |
| `GET` | `/ccapi/ver100/devicestatus/battery` | battery icon/status |
| `GET` | `/ccapi/ver100/devicestatus/temperature` | temperature warnings |
| `GET` | `/ccapi/ver100/shooting/information/recordable` | remaining shots / media state |
| `GET` | `/ccapi/ver100/event/polling?continue=on` | incremental event/status updates |
| `GET` | `/ccapi/ver100/shooting/settings/...` | current shooting settings |
| `GET` | `/ccapi/ver110/customfunctions/...` | exposure / ISO increment metadata |

### CCAPI writes used by Browser Remote

#### 1. Setting changes via `PUT {"value": ...}`

Most ordinary setting changes go through a generic helper:

```text
PUT /ccapi/.../settings/<name>
Content-Type: application/json

{"value":"..."}
```

Observed examples:

| Method | Path | JSON body shape |
| --- | --- | --- |
| `PUT` | `/ccapi/ver100/shooting/settings/tv` | `{"value":"1/125"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/av` | `{"value":"f4.0"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/exposure` | `{"value":"+1.0"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/iso` | `{"value":"800"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/wb` | `{"value":"daylight"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/picturestyle` | `{"value":"standard"}` |
| `PUT` | `/ccapi/ver100/shooting/settings/afmethod` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver100/shooting/settings/metering` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver100/shooting/settings/drive` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver100/shooting/settings/stillimageaspectratio` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver110/shooting/settings/hdr` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver110/shooting/settings/hfantiflickershoot` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver110/shooting/settings/highframerate` | `{"value":"..."}` |
| `PUT` | `/ccapi/ver110/shooting/settings/moviecropping` | `{"value":"..."}` |

#### 2. Control actions via `POST {"action": ...}`

Momentary controls use `POST` with an `action` field:

| Method | Path | JSON body shape | Meaning |
| --- | --- | --- | --- |
| `POST` | `/ccapi/ver100/shooting/control/recbutton` | `{"action":"start"}` / `{"action":"stop"}` | start/stop movie recording |
| `POST` | `/ccapi/ver100/shooting/control/moviemode` | `{"action":"on"}` / `{"action":"off"}` | switch still/movie mode |
| `POST` | `/ccapi/ver100/shooting/control/af` | `{"action":"start"}` / `{"action":"stop"}` | AF start/stop |
| `POST` | `/ccapi/ver100/shooting/control/shutterbutton/manual` | `{"action":"half_press","af":true}` | half-press shutter |
| `POST` | `/ccapi/ver100/shooting/control/shutterbutton/manual` | `{"action":"full_press","af":false}` | full press shutter |
| `POST` | `/ccapi/ver100/shooting/control/shutterbutton/manual` | `{"action":"release","af":true}` | release shutter |
| `POST` | `/ccapi/ver110/shooting/control/flickerdetection` | `{"action":"start"}` | start flicker detection |
| `POST` | `/ccapi/ver110/shooting/control/hfflickerdetection` | `{"action":"start","apply_result":...}` | HF flicker detection |

#### 3. Control values via `POST {"value": ...}`

Some controls use a value instead of an action:

| Method | Path | JSON body shape | Meaning |
| --- | --- | --- | --- |
| `POST` | `/ccapi/ver100/shooting/control/drivefocus` | `{"value":"near3"}` | drive focus |
| `POST` | `/ccapi/ver110/shooting/control/hfflickertv` | `{"value":"increment_small"}` | adjust HF flicker TV |

#### 4. Live View control

| Method | Path | JSON body / query | Meaning |
| --- | --- | --- | --- |
| `POST` | `/ccapi/ver100/shooting/liveview` | `{"liveviewsize":"medium","cameradisplay":"on"}` | enable live view mode on the camera |
| `PUT` | `/ccapi/ver100/shooting/liveview/afframeposition` | `{"positionx":123,"positiony":456}` | move touch AF point |
| `GET` | `/brapi/shooting/lvscrolldetail?liveviewsize=medium` | query only | Browser Remote live-view binary stream |
| `DELETE` | `/brapi/shooting/lvscrolldetail?liveviewsize=off` | none | Browser Remote live-view stop |

### Live View binary stream framing

The shipped `worker.js` resolves most of the live-view transport details.

Browser Remote does **not** appear to read image frames directly from
`POST /ccapi/ver100/shooting/liveview`. Instead:

1. the UI enables live view via the CCAPI control endpoint
2. it then fetches `/brapi/shooting/lvscrolldetail?liveviewsize=<size>`
3. a Web Worker parses the returned binary stream

Observed framing in `worker.js`:

- 7-byte header:
  - bytes `0..1`: start marker `0xFF00`
  - byte `2`: data type
  - bytes `3..6`: big-endian payload size
- payload: `data_size` bytes
- 2-byte footer:
  - `0xFFFF`

Accepted data types in the parser:

- `0x00`
- `0x01`
- `0x02`

The worker posts parsed chunks back to the page as:

```text
["onready"]
["onprogress", type, blob]
["oncomplete"]
["onerror", error]
```

So the current best model is:

- `/ccapi/ver100/shooting/liveview` = live-view session/control endpoint
- `/brapi/shooting/lvscrolldetail?...` = actual Browser Remote frame transport

### Event polling

`shoot_script.js` continuously polls:

```text
GET /ccapi/ver100/event/polling?continue=on
```

and reissues the request again after ~500 ms. The handler updates:

- movie mode
- recording state
- live view state
- exposure increment settings
- ordinary setting values
- media / recordable info

For future Argus Capture sessions, this is the best place to watch for camera
state changes after a control request.

## How to change settings in future sessions

This is the practical workflow to continue reverse-engineering or automate the
camera later.

### 1. Get a Browser Remote session

Use the host, port, username, and password from `argus-capture.conf`:

```sh
BASE="http://HOST:PORT"
USER="..."
PASS="..."

curl -i "$BASE/brapi/logout"
curl -i --digest -u "$USER:$PASS" "$BASE/brapi/login"
```

Capture the `brsessionid` from the successful response.

### 2. Read initial Browser Remote state

Fetch:

```sh
curl -H "Cookie: brsessionid=<id>" "$BASE/brapi/currentproperty"
```

That is the Browser Remote UI's first state snapshot for the shooting page.

### 3. Read ongoing changes

Poll:

```sh
curl -H "Cookie: brsessionid=<id>" \
  "$BASE/ccapi/ver100/event/polling?continue=on"
```

Repeat the poll after every response.

### 4. Change a setting

For ordinary settings, try a `PUT` with a `value` field:

```sh
curl -X PUT \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -d '{"value":"800"}' \
  "$BASE/ccapi/ver100/shooting/settings/iso"
```

Common pattern:

```text
PUT /ccapi/.../settings/<thing>
{"value":"..."}
```

### 5. Trigger a control action

For button-like controls, try `POST` with `action`:

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -d '{"action":"start"}' \
  "$BASE/ccapi/ver100/shooting/control/af"
```

or `POST` with `value`:

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -d '{"value":"near1"}' \
  "$BASE/ccapi/ver100/shooting/control/drivefocus"
```

### 6. Verify the result

After every write, inspect:

1. the immediate HTTP response
2. the next `/ccapi/ver100/event/polling?continue=on` result
3. if needed, a fresh `/brapi/currentproperty`

## High-value targets for future work

If Argus Capture needs deeper Browser Remote support later, start with:

1. `/brapi/currentproperty` — initial Browser Remote state model
2. `/ccapi/ver100/event/polling?continue=on` — change stream
3. `PUT /ccapi/ver100/shooting/settings/...` — ordinary setting updates
4. `POST /ccapi/ver100/shooting/control/...` — button actions
5. `/ccapi/ver100/shooting/liveview` + `/brapi/shooting/lvscrolldetail` —
   live view enable / disable path

## Reproducible curl sequence

The following is enough to reproduce the Browser Remote login flow with values
from `argus-capture.conf`:

```sh
BASE="http://HOST:PORT"
USER="..."
PASS="..."

# optional but useful if the same login is already active
curl -i "$BASE/brapi/logout"

# Browser Remote login uses Digest auth
curl -i --digest -u "$USER:$PASS" "$BASE/brapi/login"
```

Expected success pattern:

```text
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Digest realm="BrowserRemote", ...

HTTP/1.1 303 See Other
Location:/wpd/topmenu.shtml
Set-Cookie: brsessionid=<id>; Path=/; HttpOnly
```

With the returned cookie:

```sh
curl -H "Cookie: brsessionid=<id>" "$BASE/wpd/topmenu.shtml"
curl -H "Cookie: brsessionid=<id>" "$BASE/wpd/shoot.shtml"
```

## Messages worth preserving

`messages.shtml` contains the Browser Remote UI's user-facing strings. Useful
observed messages include:

- `Device busy`
- `Mode not supported`
- `Live view not started`
- `No card in camera`
- `API error`

Those strings are useful when matching UI failures back to underlying API
responses.

## Open questions

The major protocol questions are now mostly resolved.

What is still worth capturing later:

1. richer `/brapi/heartbeat` responses while recording, during FTP failure, or
   during HF flicker detection
2. actual binary samples from `/brapi/shooting/lvscrolldetail?...` to label the
   meaning of frame types `0x00`, `0x01`, and `0x02`
3. confirmation of whether any `/brapi` endpoint changes a camera setting that
   cannot already be changed through CCAPI

Based on the shipped JavaScript and the live probes captured for this document:

- `/brapi/currentproperty` is the full initial Browser Remote state snapshot
- `/brapi/heartbeat` is a lightweight JSON status delta
- `/ccapi/ver100/shooting/liveview` is a control endpoint, while
  `/brapi/shooting/lvscrolldetail?...` carries the Browser Remote live-view data
- all ordinary shooting-setting changes observed so far go through CCAPI, not
  `/brapi`
