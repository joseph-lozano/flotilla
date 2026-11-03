// The Worker: the relay's handler (src/relay.ts) and nothing else, since every name a
// Worker's main module exports is taken for an entry point.
import { handler } from "./relay";

export default handler;
