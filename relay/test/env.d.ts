import type { Env as RelayEnv } from "../src/relay";
import type { D1Migration } from "cloudflare:test";

declare global {
  namespace Cloudflare {
    interface Env extends RelayEnv {
      TEST_MIGRATIONS: D1Migration[];
      /// The fake APNs' address.
      FAKE_APNS: string;
    }
  }
}
