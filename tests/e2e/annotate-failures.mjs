/**
 * Turns Playwright's JSON report into GitHub Actions error annotations.
 *
 * Annotations are readable through the public check-runs API, so a failed CI
 * run explains itself without anyone downloading an artifact or scrolling the
 * raw log. Run from apps/desktop after a failed Playwright invocation:
 *
 *   node ../../tests/e2e/annotate-failures.mjs playwright-report/results.json
 */

import { readFileSync } from "node:fs";

const reportPath = process.argv[2];
if (!reportPath) {
  console.error("usage: node annotate-failures.mjs <playwright-results.json>");
  process.exit(1);
}

/** Collects failed test titles + first error messages from the suite tree. */
function failures(suite, acc) {
  for (const spec of suite.specs ?? []) {
    for (const test of spec.tests ?? []) {
      for (const result of test.results ?? []) {
        if (result.status === "passed" || result.status === "skipped") continue;
        const message = (result.error?.message ?? `status: ${result.status}`)
          .split("\n")
          .slice(0, 8)
          .join(" | ")
          .slice(0, 900);
        acc.push(`${spec.title}: ${message}`);
      }
    }
  }
  for (const child of suite.suites ?? []) failures(child, acc);
  return acc;
}

let report;
try {
  report = JSON.parse(readFileSync(reportPath, "utf8"));
} catch (cause) {
  console.log(`::error::could not read the Playwright report: ${String(cause)}`);
  process.exit(0);
}

const messages = failures(report, []);
for (const message of messages.slice(0, 8)) {
  // Annotations must be single-line; escape the sequences GitHub reserves.
  const safe = message.replace(/\r?\n/g, " ").replace(/%/g, "%25");
  console.log(`::error::${safe}`);
}
if (messages.length === 0) {
  console.log("::error::Playwright failed but reported no failed tests (harness-level failure)");
}
