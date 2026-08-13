// Allocate an OS-chosen free TCP port. The listener is closed before the
// port number is returned so callers can hand it to a subprocess.
export function freePort(): number {
  const l = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const { port } = l.addr as Deno.NetAddr;
  l.close();
  return port;
}
