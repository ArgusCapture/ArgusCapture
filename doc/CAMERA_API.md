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

# Camera API guide

This is the implementation-oriented camera API guide for Argus Capture.

- `doc/BROWSER_REMOTE.md` is the larger reverse-engineering notebook
- this file is the smaller working reference for the endpoints and flows we
  are actively wiring into the GTK UI

## Scope

This document tracks:

1. the Browser Remote session setup Argus Capture depends on
2. the CCAPI endpoints currently used by the UI
3. the next endpoints to use for still capture and video capture
4. which parts are verified from our codebase versus still needing camera-side
   confirmation

## Session prerequisites

Argus Capture currently assumes a Browser Remote session is established before
camera-control requests are sent.

Required inputs come from the selected camera in `argus-capture.conf`:

- host
- port
- Browser Remote username
- Browser Remote password

Current login/session model in `src/gui.rs`:

1. `GET /brapi/logout`
2. Digest-auth `GET /brapi/login`
3. capture `brsessionid=...` from `Set-Cookie`
4. `GET /wpd/shoot.shtml`
5. `GET /brapi/currentproperty`
6. live control requests carry:
   - `Cookie: brsessionid=...`
   - `Referer: http://HOST:PORT/wpd/shoot.shtml`

## Request conventions

The helper in `src/gui.rs` currently uses the same request shape for most JSON
writes:

```http
Content-Type: application/json; charset=utf-8
If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT
Cookie: brsessionid=...
Referer: http://HOST:PORT/wpd/shoot.shtml
```

That is the baseline to reuse for future control actions unless a camera model
proves it unnecessary.

## Endpoint status map

| Endpoint | Method | Current Argus Capture status | Notes |
| --- | --- | --- | --- |
| `/brapi/logout` | `GET` | implemented | clears stale Browser Remote session before login |
| `/brapi/login` | `GET` + Digest auth | implemented | returns `brsessionid` cookie on success |
| `/wpd/shoot.shtml` | `GET` | implemented | prepares the shooting page state |
| `/brapi/currentproperty` | `GET` | implemented | seeds AF mode / method and other shooting state |
| `/brapi/shooting/lvscrolldetail?liveviewsize=medium` | `GET` | implemented | live-view binary stream transport |
| `/ccapi/ver100/shooting/control/af` | `POST` | implemented | AF start / stop |
| `/ccapi/ver100/shooting/liveview/afframeposition` | `PUT` | implemented | move focus point |
| `/ccapi/ver100/shooting/control/shutterbutton` | `POST` | implemented in UI | selected for one-tap still capture; confirm on camera |
| `/ccapi/ver100/shooting/control/shutterbutton/manual` | `POST` | documented fallback | Browser Remote JS uses manual half/full/release sequence |
| `/ccapi/ver100/shooting/control/moviemode` | `POST` | next for video | switch still/movie mode |
| `/ccapi/ver100/shooting/control/recbutton` | `POST` | next for video | start / stop recording |
| `/ccapi/ver100/shooting/liveview` | `POST` | documented but not used by current code | Browser Remote uses this as a live-view control endpoint |
| `/ccapi/ver100/event/polling?continue=on` | `GET` | implemented for storage policy | used to detect newly added camera content after capture |
| `/ccapi/.../contents/...` | `GET` | implemented for storage policy | download captured file bytes using the content path returned by event polling |
| `/ccapi/.../contents/...` | `DELETE` | implemented for storage policy | remove camera copy after download when storage mode is `workspace_only` |

## Current UI-to-API mapping

| UI behavior | Source function | Endpoint(s) |
| --- | --- | --- |
| Browser Remote login | `login_browser_remote` | `/brapi/login` |
| Shoot page bootstrap | `prepare_browser_remote_shooting_page` | `/wpd/shoot.shtml`, `/brapi/currentproperty` |
| Live view stream | `stream_live_view` | `/brapi/shooting/lvscrolldetail?liveviewsize=medium` |
| Focus button | `trigger_focus` | `/ccapi/ver100/shooting/control/af` |
| Focus-point arrows | `move_focus_point` | `/ccapi/ver100/shooting/liveview/afframeposition` |
| Take Picture button | `trigger_picture_capture` | `/ccapi/ver100/shooting/control/shutterbutton` |
| Storage policy follow-up | `apply_storage_policy_to_capture` | `/ccapi/ver100/event/polling?continue=on`, `/ccapi/.../contents/...` |

## Still-photo flow

### Preferred one-tap path

The GTK capture button now targets:

```http
POST /ccapi/ver100/shooting/control/shutterbutton
{"af":true}
```

Why this is the preferred path:

- it maps cleanly to a single "Take Picture" UI action
- external CCAPI client examples use it as the normal still-capture endpoint
- it keeps the UI logic simpler than a half-press/full-press/release sequence

What still needs confirmation:

- whether the tested camera accepts this endpoint while a Browser Remote live
  session is active
- whether the response is `200 OK`, `202 Accepted`, or another success code
- whether autofocus should stay enabled in the body or become configurable

### Manual still-capture fallback

If `/shooting/control/shutterbutton` turns out to be unsupported or unreliable
on the target camera, the documented fallback is the Browser Remote-style manual
sequence:

1. `POST /ccapi/ver100/shooting/control/shutterbutton/manual`
   `{"action":"half_press","af":true}`
2. `POST /ccapi/ver100/shooting/control/shutterbutton/manual`
   `{"action":"full_press","af":false}`
3. `POST /ccapi/ver100/shooting/control/shutterbutton/manual`
   `{"action":"release","af":true}`

That sequence should be the first fallback to try before inventing a different
capture path.

## Video-control plan

The UI now exposes a picture/video mode switch, but only picture mode is wired.

The next video path to test is:

1. switch to movie mode
2. start recording
3. stop recording
4. reflect movie state back into the UI

Candidate endpoints already identified from Browser Remote:

| Purpose | Method | Endpoint | Body |
| --- | --- | --- | --- |
| enter movie mode | `POST` | `/ccapi/ver100/shooting/control/moviemode` | `{"action":"on"}` |
| leave movie mode | `POST` | `/ccapi/ver100/shooting/control/moviemode` | `{"action":"off"}` |
| start recording | `POST` | `/ccapi/ver100/shooting/control/recbutton` | `{"action":"start"}` |
| stop recording | `POST` | `/ccapi/ver100/shooting/control/recbutton` | `{"action":"stop"}` |

State fields already seen in `/brapi/currentproperty` that matter for video:

- `moviemode.status`
- `recbutton.status`
- `recordable.remainingtime`
- `recordfunctions_movie`
- `cardselection_movie`

## Storage policy behavior

The General tab now stores a `storage` mode in `argus-capture.conf`.

```ini
[ArgusCapture]
workspace = /path/to/workspace
storage = workspace_only
camera = Camera1
```

Current storage-policy behavior for still capture:

- `camera_only`: trigger capture and leave the file on the camera
- `workspace_only`: wait for the new camera content path, download it to the
  configured workspace, then delete the camera copy
- `both`: wait for the new camera content path, download it to the workspace,
  and keep the camera copy

The implementation uses `addedcontents` from event polling to find the exact
camera content path that was created by the capture. That returned content path
is then reused directly for download and optional deletion.

## cURL cookbook

### Login

```sh
BASE="http://HOST:PORT"
USER="..."
PASS="..."

curl -i "$BASE/brapi/logout"
curl -i --digest -u "$USER:$PASS" "$BASE/brapi/login"
```

### Read Browser Remote shooting state

```sh
curl -H "Cookie: brsessionid=<id>" \
  -e "$BASE/wpd/shoot.shtml" \
  "$BASE/brapi/currentproperty"
```

### Trigger autofocus

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"start"}' \
  "$BASE/ccapi/ver100/shooting/control/af"
```

### Take a picture

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"af":true}' \
  "$BASE/ccapi/ver100/shooting/control/shutterbutton"
```

### Fallback manual shutter sequence

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"half_press","af":true}' \
  "$BASE/ccapi/ver100/shooting/control/shutterbutton/manual"

curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"full_press","af":false}' \
  "$BASE/ccapi/ver100/shooting/control/shutterbutton/manual"

curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"release","af":true}' \
  "$BASE/ccapi/ver100/shooting/control/shutterbutton/manual"
```

### Enter movie mode and start recording

```sh
curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"on"}' \
  "$BASE/ccapi/ver100/shooting/control/moviemode"

curl -X POST \
  -H "Cookie: brsessionid=<id>" \
  -H "Content-Type: application/json; charset=utf-8" \
  -H "If-Modified-Since: Thu, 01 Jun 1970 00:00:00 GMT" \
  -e "$BASE/wpd/shoot.shtml" \
  -d '{"action":"start"}' \
  "$BASE/ccapi/ver100/shooting/control/recbutton"
```

## Next probes

The most useful camera-side confirmations to collect next are:

1. whether `/ccapi/ver100/shooting/control/shutterbutton` succeeds on the target
   camera during live view
2. the exact success/error body for `moviemode` and `recbutton`
3. whether `/brapi/currentproperty` is enough to refresh video state, or if
   `/ccapi/ver100/event/polling?continue=on` is required for a responsive UI
4. whether `POST /ccapi/ver100/shooting/liveview` must be added before starting
   `/brapi/shooting/lvscrolldetail` on all supported camera models
