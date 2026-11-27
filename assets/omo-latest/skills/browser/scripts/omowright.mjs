import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { guardOmowright } from "./browser-engine-guard.mjs"

export {
  BROWSER_CONFIRMATION_POLICY,
  BROWSER_ENGINE_ENV,
  BROWSER_STATE_EVENT,
  BrowserActionDeclinedError,
  BrowserEngineRefusal,
  BrowserNotConnectedError,
  BrowserUserStoppedError,
  guardOmowright,
  isIrreversibleLabel,
} from "./browser-engine-guard.mjs"

const scriptDir = dirname(fileURLToPath(import.meta.url))
const skillRoot = dirname(scriptDir)

export function resolveOmowrightEntry(env = process.env) {
  const packageRoot = join(skillRoot, "..", "..", "..", "..", "node_modules", "omowright")
  // Same layouts and order as stage-omowright-runtime.mjs: sources before the build, `.ts` before `.js`,
  // and an entry counts only together with the page bundle it reads at import time. A git install
  // carries the sources (TypeScript since omowright's strict port, which only Bun imports); a package
  // built for its `exports` carries `dist/` only.
  const layouts = [
    ["src/index.ts", "src/page-bundle.js"],
    ["src/index.js", "src/page-bundle.js"],
    ["dist/index.js", "dist/page-bundle.js"],
  ]
  const installed = layouts
    .map(([entry, pageBundle]) => [join(packageRoot, entry), join(packageRoot, pageBundle)])
    .find(([entry, pageBundle]) => existsSync(entry) && existsSync(pageBundle))
  const candidates = [
    env.OMOWRIGHT_ROOT ? join(env.OMOWRIGHT_ROOT, "index.js") : undefined,
    join(skillRoot, "runtime", "omowright", "index.js"),
    installed?.[0],
  ].filter((candidate) => candidate !== undefined)
  return candidates.find((candidate) => existsSync(candidate))
}

function kernelHost() {
  const { tool, tool_schema: toolSchema } = globalThis
  if (typeof toolSchema !== "function" || tool === undefined || tool === null) return undefined
  return {
    async listTools() {
      const listed = await toolSchema()
      return Array.isArray(listed?.tools) ? listed.tools : []
    },
    async callTool(name, args) {
      return await tool[name](args)
    },
  }
}

export async function loadOmowright(env = process.env, host = kernelHost()) {
  const entry = resolveOmowrightEntry(env)
  if (entry === undefined) {
    throw new Error(
      "omowright is not staged in this skill; run `node packages/shared-skills/stage-omowright-runtime.mjs` in a checkout, or set OMOWRIGHT_ROOT to a directory holding its bundled index.js",
    )
  }
  const raw = await import(pathToFileURL(entry).href)
  return { entry, omowright: guardOmowright(raw, { env, host }) }
}
