import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

const require = createRequire(import.meta.url);
const output = mkdtempSync(join(tmpdir(), "sdrwispy-display-tests-"));
const tests = ["display-timing", "renderers"];
try {
  writeFileSync(join(output, "package.json"), '{"type":"commonjs"}');
  execFileSync(process.execPath, [require.resolve("typescript/bin/tsc"),
    "--target", "ES2022", "--module", "commonjs", "--moduleResolution", "node",
    "--strict", "--skipLibCheck", "--outDir", output,
    ...tests.map(name => `src/${name}.test.ts`),
  ], { cwd: new URL(".", import.meta.url), stdio: "inherit" });
  for (const name of tests) {
    execFileSync(process.execPath, [join(output, `${name}.test.js`)], { stdio: "inherit" });
  }
} finally {
  rmSync(output, { recursive: true, force: true });
}
