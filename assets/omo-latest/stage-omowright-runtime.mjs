#!/usr/bin/env node
// Materialize the `browser` skill's engine runtime into the repository-owned latest overlay.
//
//   node assets/omo-latest/stage-omowright-runtime.mjs --source <omowright checkout> [--target <dir>]
//   node assets/omo-latest/stage-omowright-runtime.mjs --check --source <omowright checkout>
//
// Upstream (`packages/shared-skills/stage-omowright-runtime.mjs` as of omo v5.1.84, last changed by
// `a7c9fc4212`) bundles the root `omowright` devDependency into `skills/browser/runtime/omowright/`:
// one self-contained ESM entry, the page bundle it reads at import time, omowright's own LICENSE
// beside them, and a manifest carrying the source version, the source digest and each staged
// file's sha256. The directory is gitignored upstream and shipped through the skill's
// `.npmignore`; this repository holds no committed copy either (see
// `assets/omo-latest/runtime/.gitignore`).
//
// The materialized directory is what `bun tools/package-native.mjs --skill-source latest
// --omowright-runtime <dir>` stages beside the `browser` skill. Its manifest is the runtime's
// identity proof: `package-native` refuses a runtime whose recorded `commit` is not the commit the
// overlay declares, so a foreign or stale runtime cannot be staged silently.
//
// Immutable identity: the source checkout must be the pinned omowright revision. `--identity <40hex>`
// defaults to the `browserRuntime.dependency.commit` recorded in `assets/omo-latest/manifest.json`,
// and the checkout's `git rev-parse HEAD` must equal it. Nothing here is downloaded: the operator
// supplies the checkout (e.g. the `node_modules/omowright` git dependency of a pinned oh-my-openagent
// checkout, or a direct clone of the pinned commit).
//
// This is packaging of upstream skill content only. It adds no desktop/browser implementation, no
// engine authorization and no extension bridge: the bundled file is the same upstream library
// upstream ships, and `dec-a-02` (browsers, engines, permission protocol) stays an owner choice.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { access, copyFile, mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const overlayRoot = scriptDir;
const overlayManifestPath = join(overlayRoot, "manifest.json");
const defaultTargetDir = join(overlayRoot, "runtime", "browser", "omowright");

/** The runtime files bundled or copied from the source and hashed into the runtime manifest, in the order upstream stages them. LICENSE is omowright's own MIT text, shipped beside the bundle (omowright#290). */
export const STAGED_FILES = Object.freeze(["index.js", "page-bundle.js", "LICENSE"]);
/** The runtime manifest upstream's `stage-omowright-runtime.mjs` writes. */
export const RUNTIME_MANIFEST_NAME = "manifest.json";
// An omowright checkout has one of three layouts. A git checkout (what the release workflows
// materialize from) carries the sources: TypeScript since omowright's strict port, JavaScript before
// it. A package built for its `exports` carries a compiled `dist/` and no sources. Sources come before
// the build (a checkout that ran its own build would otherwise stage a stale dist) and `.ts` before
// `.js`; the browser skill's loader (`skills/browser/scripts/omowright.mjs`) walks the same order, so
// the two never pick different entries. Entry names are "/"-joined: they are hashed into the digest,
// which must not depend on the OS.
export const OMOWRIGHT_LAYOUTS = Object.freeze([
	Object.freeze({ entry: "src/index.ts", pageBundle: "src/page-bundle.js", sources: Object.freeze(["src/core.ts"]) }),
	Object.freeze({ entry: "src/index.js", pageBundle: "src/page-bundle.js", sources: Object.freeze(["src/core.js"]) }),
	Object.freeze({ entry: "dist/index.js", pageBundle: "dist/page-bundle.js", sources: Object.freeze([]) }),
]);

const COMMIT_PATTERN = /^[0-9a-f]{40}$/;

function usage() {
	console.log(`usage: node assets/omo-latest/stage-omowright-runtime.mjs --source <omowright checkout> [--target <dir>] [--identity <40-hex commit>] [--bun <path>]
       node assets/omo-latest/stage-omowright-runtime.mjs --check --source <omowright checkout> [--target <dir>] [--identity <40-hex commit>]

  --source <dir>     the omowright package checkout to bundle (or $OMO_OMOWRIGHT_SOURCE)
  --target <dir>     where the materialized runtime is written (default: <overlay>/runtime/browser/omowright)
  --identity <commit>  the immutable omowright commit the source must be at (default: the overlay manifest's browserRuntime.dependency.commit)
  --bun <path>       the bun executable used for bun build (or $BUN; default: bun)
  --check            freshness gate: fail when the staged runtime does not match the source`);
}

async function sha256(path) {
	return createHash("sha256").update(await readFile(path)).digest("hex");
}

/** Upstream's `readSourceVersion`: the source must be the omowright package. */
async function readSourceVersion(sourceRoot) {
	let manifest;
	try {
		manifest = JSON.parse(await readFile(join(sourceRoot, "package.json"), "utf8"));
	} catch (error) {
		throw new Error(`omowright source is not a package checkout (${join(sourceRoot, "package.json")}): ${error.message}`);
	}
	if (manifest.name !== "omowright" || typeof manifest.version !== "string" || manifest.version.length === 0) {
		throw new Error(`omowright source is not the omowright package: ${sourceRoot}`);
	}
	return manifest.version;
}

/** Upstream's `resolveLayout`: the first layout whose entry and page bundle both exist. */
function resolveLayout(sourceRoot) {
	const layout = OMOWRIGHT_LAYOUTS.find((candidate) => existsSync(join(sourceRoot, candidate.entry)) && existsSync(join(sourceRoot, candidate.pageBundle)));
	if (layout === undefined) {
		const looked = OMOWRIGHT_LAYOUTS.map((candidate) => candidate.entry).join(", ");
		throw new Error(`omowright has no stageable layout under ${sourceRoot}: looked for ${looked}`);
	}
	return layout;
}

/** Upstream's `sourceDigest`: the named source entries, in order, length-prefixed with NUL. */
async function sourceDigest(sourceRoot, layout) {
	const hash = createHash("sha256");
	for (const entry of ["package.json", "LICENSE", layout.entry, layout.pageBundle, ...layout.sources]) {
		let content;
		try {
			content = await readFile(join(sourceRoot, entry));
		} catch (error) {
			throw new Error(`omowright source is missing ${entry} (${join(sourceRoot, entry)}): ${error.message}`);
		}
		hash.update(entry).update("\0").update(content);
	}
	return hash.digest("hex");
}

/** The source checkout's actual commit, or null when it is not a git checkout. */
function gitHead(directory) {
	const result = spawnSync("git", ["-C", directory, "rev-parse", "HEAD"], { encoding: "utf8" });
	if (result.status !== 0) return null;
	const sha = String(result.stdout ?? "").trim();
	return COMMIT_PATTERN.test(sha) ? sha : null;
}

/** The overlay's declared omowright commit, read from the machine-consumed overlay manifest. */
async function overlayIdentityCommit() {
	const manifest = JSON.parse(await readFile(overlayManifestPath, "utf8"));
	const commit = manifest?.browserRuntime?.dependency?.commit;
	if (typeof commit !== "string" || !COMMIT_PATTERN.test(commit)) {
		throw new Error(`overlay manifest does not declare browserRuntime.dependency.commit: ${overlayManifestPath}`);
	}
	return commit;
}

/** Upstream's `bundle`: one self-contained ESM entry built from the omowright source. */
function bundle({ sourceRoot, layout, outFile, bunExecutable }) {
	const result = spawnSync(
		bunExecutable,
		["build", join(sourceRoot, layout.entry), "--target=node", "--format=esm", "--minify-whitespace", `--outfile=${outFile}`],
		{ cwd: sourceRoot, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
	);
	if (result.status !== 0) throw new Error(`bun build failed for omowright: ${result.stderr || result.stdout}`);
}

async function resolveIdentity(explicit) {
	const identity = explicit ?? (await overlayIdentityCommit());
	if (!COMMIT_PATTERN.test(identity)) throw new Error(`--identity must be a full 40-hex commit (found ${JSON.stringify(identity)})`);
	return identity;
}

export async function stageOmowrightRuntime(options = {}) {
	const sourceRoot = resolve(options.sourceRoot ?? process.env.OMO_OMOWRIGHT_SOURCE ?? "");
	const targetDir = resolve(options.targetDir ?? defaultTargetDir);
	const bunExecutable = options.bunExecutable ?? process.env.BUN ?? "bun";
	const identity = await resolveIdentity(options.identity);

	const version = await readSourceVersion(sourceRoot);
	const layout = resolveLayout(sourceRoot);
	if (!existsSync(join(sourceRoot, "LICENSE"))) throw new Error(`omowright has no LICENSE to ship beside the bundle: ${join(sourceRoot, "LICENSE")}`);
	const digest = await sourceDigest(sourceRoot, layout);
	const head = gitHead(sourceRoot);
	if (head === null) throw new Error(`omowright source is not a git checkout, so its immutable identity cannot be proven: ${sourceRoot}`);
	if (head !== identity) throw new Error(`omowright source is at ${head}, not the declared commit ${identity}`);

	await mkdir(dirname(targetDir), { recursive: true });
	const tempParent = await mkdtemp(join(dirname(targetDir), ".tmp-omowright-runtime-"));
	const tempDir = join(tempParent, "omowright");
	const backupDir = `${targetDir}.backup-${process.pid}-${Date.now()}`;
	let backupCreated = false;
	let targetMoved = false;
	try {
		await mkdir(tempDir, { recursive: true });
		bundle({ sourceRoot, layout, outFile: join(tempDir, "index.js"), bunExecutable });
		await copyFile(join(sourceRoot, layout.pageBundle), join(tempDir, "page-bundle.js"));
		await copyFile(join(sourceRoot, "LICENSE"), join(tempDir, "LICENSE"));
		const files = {};
		for (const name of STAGED_FILES) files[name] = await sha256(join(tempDir, name));
		await writeFile(
			join(tempDir, RUNTIME_MANIFEST_NAME),
			`${JSON.stringify({ name: "omowright", version, commit: identity, sourceDigest: digest, files, stagedAtUtc: new Date().toISOString() }, null, 2)}\n`,
			"utf8",
		);
		try {
			await access(targetDir);
			await rename(targetDir, backupDir);
			backupCreated = true;
		} catch (error) {
			if (error.code !== "ENOENT") throw error;
		}
		await rename(tempDir, targetDir);
		targetMoved = true;
		if (backupCreated) await rm(backupDir, { recursive: true, force: true });
		return { ok: true, sourceRoot, targetDir, version, commit: identity, sourceDigest: digest, files };
	} catch (error) {
		if (!targetMoved && backupCreated) {
			await rm(targetDir, { recursive: true, force: true });
			await rename(backupDir, targetDir);
		}
		throw error;
	} finally {
		await rm(tempParent, { recursive: true, force: true });
	}
}

export async function checkOmowrightRuntimeFresh(options = {}) {
	const sourceRoot = resolve(options.sourceRoot ?? process.env.OMO_OMOWRIGHT_SOURCE ?? "");
	const targetDir = resolve(options.targetDir ?? defaultTargetDir);
	const identity = await resolveIdentity(options.identity);
	const digest = await sourceDigest(sourceRoot, resolveLayout(sourceRoot));
	const head = gitHead(sourceRoot);
	if (head === null) throw new Error(`omowright source is not a git checkout, so its immutable identity cannot be proven: ${sourceRoot}`);
	if (head !== identity) throw new Error(`omowright source is at ${head}, not the declared commit ${identity}`);
	let manifest;
	try {
		manifest = JSON.parse(await readFile(join(targetDir, RUNTIME_MANIFEST_NAME), "utf8"));
	} catch (error) {
		if (error.code === "ENOENT") throw new Error(`omowright runtime stale: manifest is missing: ${join(targetDir, RUNTIME_MANIFEST_NAME)}`);
		throw new Error(`omowright runtime stale: manifest is unreadable: ${error.message}`);
	}
	if (manifest.commit !== identity) throw new Error(`omowright runtime stale: staged commit ${manifest.commit} does not match ${identity}`);
	if (manifest.sourceDigest !== digest) throw new Error(`omowright runtime stale: staged sourceDigest ${manifest.sourceDigest} does not match ${digest}`);
	for (const name of STAGED_FILES) {
		let actual;
		try {
			actual = await sha256(join(targetDir, name));
		} catch (error) {
			if (error.code === "ENOENT") throw new Error(`omowright runtime stale: ${name} is missing from ${targetDir}`);
			throw error;
		}
		if (manifest.files?.[name] !== actual) throw new Error(`omowright runtime stale: ${name} sha256 ${actual} does not match manifest ${manifest.files?.[name]}`);
	}
	return { ok: true, sourceRoot, targetDir, version: manifest.version, commit: identity, sourceDigest: digest };
}

function parseArgs(argv) {
	const options = { sourceRoot: null, targetDir: null, identity: null, bunExecutable: null, check: false };
	for (let cursor = 0; cursor < argv.length; cursor += 1) {
		const argument = argv[cursor];
		const next = () => {
			cursor += 1;
			if (cursor >= argv.length) throw new Error(`${argument} needs a value`);
			return argv[cursor];
		};
		if (argument === "--source") options.sourceRoot = next();
		else if (argument === "--target") options.targetDir = next();
		else if (argument === "--identity") options.identity = next();
		else if (argument === "--bun") options.bunExecutable = next();
		else if (argument === "--check") options.check = true;
		else if (argument === "--help" || argument === "-h") {
			usage();
			process.exit(0);
		} else throw new Error(`unknown argument: ${argument}`);
	}
	return options;
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
	try {
		const options = parseArgs(process.argv.slice(2));
		if (options.check) {
			const result = await checkOmowrightRuntimeFresh(options);
			console.log(`omowright runtime is current: ${result.targetDir} (omowright ${result.version} @ ${result.commit})`);
		} else {
			const result = await stageOmowrightRuntime(options);
			console.log(`staged omowright ${result.version} (${result.commit}) into ${result.targetDir}`);
		}
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exit(1);
	}
}
