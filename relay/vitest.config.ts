import { defineConfig } from "vitest/config";
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { TEST_P8, TEST_KEY_ID, TEST_TEAM_ID } from "./test/fake-apns.mjs";

export default defineConfig(async () => {
  const migrations = await readD1Migrations("./migrations");
  return {
    plugins: [
      cloudflareTest(({ inject }) => {
        const apns = `http://127.0.0.1:${inject<number>("apnsPort")}`;
        return {
          wrangler: { configPath: "./wrangler.toml" },
          miniflare: {
            bindings: {
              TEST_MIGRATIONS: migrations,
              APNS_KEY: TEST_P8,
              APNS_KEY_ID: TEST_KEY_ID,
              APNS_TEAM_ID: TEST_TEAM_ID,
              APNS_ORIGIN_SANDBOX: `${apns}/sandbox`,
              APNS_ORIGIN_PRODUCTION: `${apns}/production`,
              FAKE_APNS: apns,
              SELFTEST_TOKEN: "selftest-token-for-tests",
            },
          },
        };
      }),
    ],
    test: {
      globalSetup: ["./test/global-setup.mjs"],
      setupFiles: ["./test/apply-migrations.ts"],
    },
  };
});
