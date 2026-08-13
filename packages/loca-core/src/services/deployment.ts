// Deployment posture, read once at boot (see main.ts).
//
// `behindProxy` marks that Core is served over HTTPS by a terminating reverse
// proxy (Cloudflare, Caddy). When set, the Client reaches the proxy's real
// CA-signed certificate rather than Core's self-signed one, so it validates via
// standard WebPKI instead of pinning, and the pairing handshake drops the cert
// binding (the route folds an empty pin, see http/routes/pairing.ts).
//
// Like `server.bindHost`, it is read raw from settings.json / an env var and is
// deliberately NOT an API-writable schema setting: a paired (or compromised)
// client must not be able to weaken Core's pairing posture. The only ways to set
// it are the local settings.json or the `LOCA_CORE_BEHIND_PROXY` env var.

let behindProxy = false;

export function setBehindProxy(value: boolean): void {
  behindProxy = value;
}

export function isBehindProxy(): boolean {
  return behindProxy;
}
