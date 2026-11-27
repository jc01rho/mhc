// Single source of truth for the upstream senpi commits this workspace targets.
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";

export const SENPI_PIN = "f01af134919f0bbf24b2887000e2142dfd290567";
/** senpi v2026.11.23: the commit crates/maho-ai/data/models.json is generated from (docs/divergences.md CAT-1). */
export const SENPI_CATALOG_PIN = "47333c26cdba96631d64317284a06ca6e0aab68e";

/** Returns the absolute SENPI_SRC path, or exits 3 when it is not a checkout at `pin`. */
export function pinnedSenpiRoot(pin = SENPI_PIN) {
	const root = resolve(process.env.SENPI_SRC ?? "/Users/indo/code/senpi");
	let head;
	try {
		head = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], {
			encoding: "utf8",
			stdio: ["ignore", "pipe", "pipe"],
		}).trim();
	} catch (err) {
		console.error(`SENPI_SRC (${root}) is not a git repository: ${err.message}`);
		process.exit(3);
	}
	if (head !== pin) {
		console.error(`SENPI_SRC (${root}) is at ${head}, expected pin ${pin}`);
		process.exit(3);
	}
	return root;
}
