// Vitest's global setup, in Node: the fake APNs the Worker under test sends to.
import { start } from "./fake-apns.mjs";

export default async function ({ provide }) {
  const server = await start();
  provide("apnsPort", server.address().port);
  return () => new Promise((resolve) => server.close(resolve));
}
