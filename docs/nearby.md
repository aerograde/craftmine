# Play Nearby

Local multiplayer with no server of ours: players on the same network (or the
same NAT) find each other, ask to join a world or invite someone to it, and
play peer to peer over WebRTC. MQTT carries only presence and signalling; the
game's bytes never touch a broker.

## What it does

- **Discovery.** A singleplayer world is announced automatically when it
  opens (the pause menu's Play Nearby screen can stop sharing it). Nearby
  players see the world, its host, players and join policy. Players who are
  not in a world are announced too, so a host can invite them.
- **Join requests.** A guest asks to join; the host sees the request on the
  Play Nearby screen, on a toast and in a clickable chat line, and accepts or
  denies it. The default policy is *Ask to join*; *Anyone nearby* accepts
  without a prompt, *Invite only* accepts only a host's invitation.
- **Invites.** The host picks a nearby player and invites them; the player
  gets a toast and a screen entry to join. The invitation carries a one-time
  token.
- **Sessions.** Each accepted guest gets its own WebRTC data channel to the
  host's integrated server (a star of independent links, so one slow guest
  does not hold up another). Up to the world's normal LAN player count.
- **The server stays up while the host pauses.** Connected nearby guests keep
  the integrated server ticking, as LAN guests do.

## How it fits together

```
presence + sealed signalling                  WebRTC data channel
LAN multicast 224.0.2.60:4446  ─┐
                                 ├─► host / guest  ──► SPAKE2 tunnel ──► memory Pipe
public MQTT brokers             ─┘        (DTLS + SCTP)                 (game's socket)
```

- Presence is public: a random id, the player or world name, the join policy,
  player count and the sender's ephemeral X25519 public key. It contains no
  IP addresses.
- Join requests, accepts, offers and answers are sealed to the recipient's
  key (or the session's key). A broker relays opaque blobs.
- The data channel then runs the same SPAKE2 + ChaCha20-Poly1305 tunnel as
  Play with Friends (`horizon-friends::crypto`), keyed by the accepted
  one-time token. `horizon-nearby::pump` carries the game's memory `Pipe`
  through it, so the game's networking code is unchanged.

The pieces:

| Crate | Owns |
|---|---|
| `horizon-webrtc` | Native WebRTC (str0m): ICE with STUN, DTLS/SCTP, data channels, mDNS candidate resolution |
| `horizon-nearby` | Presence, LAN multicast, MQTT client and broker pool, signalling, join requests and invites, the session state machine, the secure pump |
| `horizon-os` | The browser's `RTCPeerConnection` bridge and MQTT-over-WebSocket |
| `horizon-client` | The Play Nearby screen, the title button, the pause-menu icon, auto-sharing, the integrated-server pipes |

### Discovery

Two channels run at once and are merged:

1. **LAN multicast** on `224.0.2.60:4446` (vanilla's pinger stays on 4445).
   Presence is a `HZNB` frame with JSON; signalling too large for one datagram
   is split into numbered chunks and sent twice. This works with the internet
   unplugged.
2. **MQTT** through the public brokers below. The topic room is:
   - `auto-<hash>` of the public address STUN (or the browser's HTTP lookup)
     reports, so everyone behind one NAT shares a room; or
   - `lan-<hash>` of the private networks when STUN fails; or
   - `nearby-public` when neither is available (a single-threaded browser has
     no WebRTC and cannot enumerate LAN interfaces), so the brokers still
     start; or
   - a code the player sets (`nearbyRoom` or the screen's code), for a VPN or
     two different networks.

   Presence is published to every reachable broker, so a player on one broker
   still finds a host on another. World presence is retained; player presence
   is retained and refreshed every 10 s. The MQTT will clears both if the
   process dies; stale records expire after 35 s.

The default brokers, in order:

| Broker | Native | Browser |
|---|---|---|
| EMQX | `mqtts://broker.emqx.io:8883` | `wss://broker.emqx.io:8084/mqtt` |
| Mosquitto test (Eclipse) | `mqtts://test.mosquitto.org:8883` | `wss://test.mosquitto.org:8081` |

The old `mqtt.eclipseprojects.io` host is deliberately not used: it resolves
but no longer accepts TLS (8883) or HTTPS (443), so it was a dead half of
every pair. `test.mosquitto.org` is the live Eclipse Mosquitto broker.
HiveMQ is not in the pool.

No other external service is used. STUN is Google's (`stun.l.google.com`,
`stun1`, `stun2`, with `stun.cloudflare.com` as a fallback). There is no TURN.
The threaded browser finds its public address with a throwaway
`RTCPeerConnection`; the single-threaded page, which has no WebRTC, gets an
HTTP lookup from `main.js` (`api64.ipify.org`); if it has not arrived by the
time Nearby starts, the public room is used.

### A join

1. The guest's service looks up the world's host id and static key from
   presence, makes an ephemeral key pair and seals a `JoinRequest` to the
   host. It goes over LAN unicast and every broker.
2. The host opens it, applies the world's policy, and either asks the player
   or accepts. An acceptance carries the host's ephemeral key and a one-time
   token, sealed to the guest's static key.
3. Both derive a session key from the two ephemeral keys. The guest creates
   an `RTCPeerConnection` and a data channel, sends its offer sealed with the
   session key; the host answers.
4. When the channel opens, both run the SPAKE2 exchange with the token, then
   the encrypted record layer. The host connects the tunnel to the integrated
   server through a memory `Pipe`; the guest hands its half to the game's
   normal `connect_local`, which is how a LAN guest joins too.

The invite flow is the same, except the host starts it with a token; the
guest's request carries that token and is accepted without a second prompt.

### Browser notes

The browser build uses its own `RTCPeerConnection` and MQTT over WebSockets.
It has no UDP sockets, so:
- LAN multicast is unavailable; a broker is required for browser↔browser and
  browser↔native discovery on the same network.
- The browser's ICE candidates for local addresses are mDNS names; native
  peers resolve them before adding the candidate.
- The no-threads build (`THREADS=0 web/build.sh`) has no networking, so Play
  Nearby reports itself off.

## Security and privacy

- The MQTT brokers see presence metadata (names, world names, policy, player
  count) and sealed signalling. They never see game traffic.
- Signalling is sealed with ephemeral X25519 + HKDF + ChaCha20-Poly1305. The
  data channel is additionally keyed by the accepted one-time token and the
  SPAKE2 exchange, so a relay or broker that did not take part in the accept
  cannot open the tunnel.
- A public broker is a public place: anyone who knows the room hash can see
  the names. Turn sharing off (the screen's Stop, or `nearbyEnabled:false`),
  or use a custom room code, if that matters.
- Nothing is persisted: ids and keys are new each run.

## Configuration

`horizon.txt` beside `options.txt` (or environment overrides):

| Key | Default | Meaning |
|---|---|---|
| `nearbyEnabled` | `true` | Start the service at all |
| `nearbyShare` | `true` | Share the open singleplayer world automatically |
| `nearbyPolicy` | `ask` | `ask`, `open` or `invite` |
| `nearbyRoom` | automatic | A custom room code (4-16 letters/digits) |
| `nearbyBrokers` | the table above | Comma-separated `mqtt(s)://` or `ws(s)://` URLs |
| `nearbyStun` | Google + Cloudflare | Comma-separated `host:port` STUN servers |

Tests and automation:

| Variable | Meaning |
|---|---|
| `HORIZON_NEARBY=0` | Disable the service |
| `HORIZON_NEARBY_BROKERS` | Broker URLs, comma-separated |
| `HORIZON_NEARBY_STUN` | STUN servers |
| `HORIZON_NEARBY_ROOM` | A fixed room code |
| `HORIZON_NEARBY_POLICY` | `ask`, `open` or `invite` |
| `HORIZON_NEARBY_SHARE=0` | Do not share automatically |
| `HORIZON_NEARBY_LAN=0` | Do not use LAN multicast |
| `HORIZON_NEARBY_LOOPBACK=1` | Bind and advertise 127.0.0.1 (two instances on one machine) |

The pause menu's Play Nearby screen raises and lowers sharing, cycles the
join policy, copies the room code, and shows requests, invites, nearby worlds
and nearby players. `/nearby` (from a clicked chat line) opens it.

## Command line

`horizon-ctl nearby ...`:

```
nearby status                 the room, brokers, worlds, people, requests, sessions
nearby share | stop           raise or lower sharing
nearby join <world-id>        ask to join
nearby accept|deny <request>  answer a request
nearby invite <player-id>     invite a nearby player
nearby accept-invite <id>     take an invite
nearby code <code>            join a custom room
```

## Limits

- No TURN. Two symmetric NATs, or a locked-down network that blocks UDP, may
  not find a path. The session reports that instead of hanging; the existing
  Play with Friends invite code is the fallback for those players.
- A dedicated server (`--dedicated`) and the server thread itself do not run
  the service; Play Nearby shares a window's singleplayer world.
- The public brokers are best-effort. LAN multicast keeps working if they are
  all down, but browser peers need one.
- Browser peers on a LAN depend on a broker for discovery (no multicast), and
  cross-play relies on mDNS resolution of the browser's host candidates.

## Tests

```sh
cargo test -p horizon-webrtc                       # the native peer over loopback
cargo test -p horizon-nearby                       # presence, MQTT codec, LAN codec, sealing, pump
cargo test -p horizon-nearby --test service        # two/three services through a fake broker:
                                                   # discovery, request, invite, two guests,
                                                   # WebRTC, SPAKE2 and bytes through the pipes
cargo +1.95 test -p horizon-client --lib nearby_screen
```

The service test runs a fake MQTT 3.1.1 broker in the test process and talks
to 127.0.0.1 only; it needs no game assets and no network.
