// Generic HTTP fake sidecar used as a stand-in for any sidecar whose
// readiness check is a 200 on /health (and that doesn't otherwise need to
// speak its real protocol within a test).
//
// Usage:  deno run --allow-net=127.0.0.1 http-stub.ts <port>
// The process serves a 200 "ok" on every path and writes "READY" to stdout
// once the listener is bound, matching the smoke-test contract. A request to
// /exit makes the process exit non-zero, letting tests simulate an unexpected
// sidecar crash.

const port = Number(Deno.args[0]);
if (!Number.isInteger(port) || port <= 0 || port > 65535) {
  console.error(`http-stub: invalid port ${Deno.args[0]}`);
  Deno.exit(2);
}

Deno.serve({ port, hostname: "127.0.0.1" }, (req) => {
  if (new URL(req.url).pathname === "/exit") {
    // Flush the response, then crash on the next tick so the test's fetch
    // doesn't always reject before it sees a reply.
    setTimeout(() => Deno.exit(1), 0);
    return new Response("exiting");
  }
  return new Response("ok");
});
console.log("READY");
