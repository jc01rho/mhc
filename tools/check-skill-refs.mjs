#!/usr/bin/env bun
// SPDX-License-Identifier: MIT
// tools/check-skill-refs.mjs - 3-pass dangling skill, agent/category, and path reference validator (omo #12484, #12528, #12614, #12615).
//
// Pass 1 (skill-name): skill names in load_skills, skill(...), /skill:<name>, skills/<name>/SKILL.md, `<name>` skill
// Pass 2 (agent-name / category-name): subagent_type, agent_type, and category inside task(...) calls
// Pass 3 (skill-path / ulw-bench rules): skill-relative references/scripts/templates/.. paths, repo-relative paths, report-tools subcommands, eval denylist

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { checkFile as checkUlwBenchFile } from "./ulw-bench/check-skill-refs.mjs";

const PLACEHOLDER_NAMES = new Set([
  "...",
  "a",
  "b",
  "example",
  "foo",
  "item-name",
  "my-skill",
  "name",
  "skill-1",
  "skill-2",
  "skill-name",
  "your-skill",
  "selected",
  "skill",
  "parent",
  "companion",
  "x",
  "y",
]);

const BUILTIN_EXTRA_SKILLS = [
  "bun",
  "bun-1-4",
  "computer-use",
  "gpt-image-gen",
  "imagegen",
  "mcp",
  "pdf",
  "rules",
  "side-panel",
  "skills",
  "webfetch",
  "workpool",
  "x-search",
];

const OPTIONAL_ECOSYSTEM_SKILLS = [
  "agent-browser",
  "dev-browser",
  "mass-ulw",
  "openai-docs",
  "playwright",
  "playwright-cli",
  "review-agent",
  "security-research",
  "security-review",
  "skill-creator",
  "skill-installer",
  "team-mode",
  "teammode",
];

export const KNOWN_AGENTS = [
  "atlas",
  "default",
  "designer",
  "explore",
  "explorer",
  "hephaestus",
  "implementation-worker",
  "librarian",
  "metis",
  "momus",
  "omo-native-code-reviewer",
  "omo-native-gate-reviewer",
  "omo-native-qa-executor",
  "omo-senpi-code-reviewer",
  "omo-senpi-gate-reviewer",
  "omo-senpi-qa-executor",
  "oracle",
  "plan",
  "plan-consultant",
  "plan-reviewer",
  "planner",
  "prometheus",
  "sisyphus",
  "sisyphus-junior",
  "verification-worker",
  "visual-worker",
  "worker",
];

export const KNOWN_CATEGORIES = [
  "architect",
  "artistry",
  "deep",
  "deep-high",
  "deep-low",
  "git",
  "memory-reflect",
  "quick",
  "reverse-engineering",
  "ultrabrain",
  "unspecified-high",
  "unspecified-low",
  "visual-engineering",
  "writing",
];

const SKILL_NAME_SHAPE = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
const TEXT_FILE = /\.(md|mdx|txt|mjs|js|ts|json|ya?ml|toml)$/;
const NON_SHIPPED_FILE =
  /(?:\.test\.|\.spec\.|(?:^|\/)(?:test|tests|__tests__|fixtures|designpowers)\/|(?:^|\/)AGENTS\.md$|(?:^|\/)ATTRIBUTION\.md$|(?:^|\/)CLEANROOM\.md$|(?:^|\/)EVIDENCE\.md$|(?:^|\/)changes\.md$|(?:^|\/)CHANGELOG\.md$)/;

function toPortable(p) {
  return p.split(sep).join("/");
}

function listDirectories(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => e.name)
    .sort();
}

function listFilesRecursive(dir) {
  if (!existsSync(dir)) return [];
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "dist" || entry.name === "target" || entry.name === ".git") continue;
      out.push(...listFilesRecursive(full));
    } else if (entry.isFile()) {
      out.push(full);
    }
  }
  return out;
}

export function readSkillName(skillFile) {
  const content = readFileSync(skillFile, "utf8").replace(/\r\n?/g, "\n");
  const frontmatter = content.match(/^---\n([\s\S]*?)\n---/)?.[1];
  const name = frontmatter?.match(/^name:\s*["']?([^"'\n]+?)["']?\s*$/m)?.[1]?.trim();
  return name ?? basename(dirname(skillFile));
}

export function buildReferenceCatalog(repoRoot) {
  const skillsDir = join(repoRoot, "assets/omo-latest/skills");
  const skillRoots = new Map();
  const shippedSkills = new Set(BUILTIN_EXTRA_SKILLS);

  for (const dirName of listDirectories(skillsDir)) {
    const skillRoot = join(skillsDir, dirName);
    const skillMd = join(skillRoot, "SKILL.md");
    if (!existsSync(skillMd)) continue;
    const declaredName = readSkillName(skillMd);
    shippedSkills.add(declaredName);
    shippedSkills.add(dirName);
    skillRoots.set(declaredName, [...(skillRoots.get(declaredName) ?? []), skillRoot]);
    if (dirName !== declaredName) {
      skillRoots.set(dirName, [...(skillRoots.get(dirName) ?? []), skillRoot]);
    }
  }

  const optionalSkills = new Set([...shippedSkills, ...OPTIONAL_ECOSYSTEM_SKILLS]);
  const agents = new Set(KNOWN_AGENTS);
  const categories = new Set(KNOWN_CATEGORIES);

  return {
    shippedSkills,
    optionalSkills,
    agents,
    categories,
    skillRoots,
  };
}

function normalizeSeparators(name) {
  return name.toLowerCase().replaceAll("_", "-").replace(/-{2,}/g, "-");
}

/** Optimal-string-alignment distance (Damerau-Levenshtein with adjacent transpositions). */
export function editDistance(left, right) {
  const table = Array.from({ length: left.length + 1 }, (_, row) =>
    Array.from({ length: right.length + 1 }, (_, col) => (row === 0 ? col : col === 0 ? row : 0)),
  );
  for (let row = 1; row <= left.length; row += 1) {
    for (let col = 1; col <= right.length; col += 1) {
      const cost = left[row - 1] === right[col - 1] ? 0 : 1;
      let best = Math.min(
        table[row - 1][col] + 1,
        table[row][col - 1] + 1,
        table[row - 1][col - 1] + cost,
      );
      if (
        row > 1 &&
        col > 1 &&
        left[row - 1] === right[col - 2] &&
        left[row - 2] === right[col - 1]
      ) {
        best = Math.min(best, table[row - 2][col - 2] + 1);
      }
      table[row][col] = best;
    }
  }
  return table[left.length][right.length];
}

export function suggestName(reference, candidates) {
  const sorted = [...new Set(candidates)].sort();
  const normalized = normalizeSeparators(reference);
  const separatorMatch = sorted.find((c) => normalizeSeparators(c) === normalized);
  if (separatorMatch !== undefined) return separatorMatch;
  const limit = normalized.length <= 4 ? 1 : 2;
  let best;
  for (const candidate of sorted) {
    const distance = editDistance(normalized, normalizeSeparators(candidate));
    if (distance <= limit && (best === undefined || distance < best.distance)) {
      best = { name: candidate, distance };
    }
  }
  return best?.name;
}

export function suggestPath(root, fromDirectory, reference) {
  if (!root || !existsSync(root)) return undefined;
  const target = (reference.split("#", 1)[0] ?? reference).replace(/^\.\//, "");
  const candidates = listFilesRecursive(root).map((file) => toPortable(relative(root, file)));
  const sameName = candidates.filter((c) => basename(c) === basename(target));
  const local = toPortable(relative(root, fromDirectory));
  const sameDirectory = sameName.filter((c) => dirname(c) === local);
  if (sameDirectory.length === 1) return sameDirectory[0];
  if (sameName.length === 1) return sameName[0];
  let best;
  for (const candidate of candidates) {
    const distance = editDistance(target, candidate);
    if (distance <= 3 && (best === undefined || distance < best.distance)) {
      best = { path: candidate, distance };
    }
  }
  return best?.path;
}

const LOAD_SKILLS_LIST = /load_skills["']?\s*[=:]\s*\[([^\]\n]*)\]/g;
const QUOTED = /["']([^"'\s]+)["']/g;
const SKILL_TOOL_CALL = /\bskill\s*\(\s*(?:name\s*[=:]\s*)?["']([^"']+)["']/g;
const SLASH_SKILL = /\/skill:([A-Za-z0-9][A-Za-z0-9_-]*)/g;
const SKILL_FILE_PATH = /(?:^|[\s`'"(/])skills\/([A-Za-z0-9][A-Za-z0-9_-]*)\/SKILL\.md/g;
const BACKTICK_THEN_SKILL = /`\/?([a-z0-9][a-z0-9_-]*)`\s+skill\b/g;
const SKILL_THEN_BACKTICK = /\b[Ss]kill\s+`\/?([a-z0-9][a-z0-9_-]*)`/g;
const SUBAGENT_TYPE = /["']?subagent_type["']?\s*[=:]\s*["']([A-Za-z0-9_.-]+)["']/g;
const AGENT_TYPE = /(?<![A-Za-z_])["']?agent_type["']?\s*[=:]\s*["']([A-Za-z0-9_.-]+)["']/g;
const CATEGORY = /["']?category["']?\s*[=:]\s*["']([A-Za-z0-9_.-]+)["']/g;
const TASK_CALL_OPEN = /\b(?:task|call_omo_agent|delegate_task)\s*\(/g;
const SKILL_RELATIVE_PATH = /`(\.{1,2}\/[^`\s]+\.md|(?:references|scripts|assets|templates)\/[^`\s]+)`/g;
const SKILL_OWNER = /`([a-z0-9][a-z0-9-]*)`(?:'s\b|\s+skill\b)|\bload\s+`([a-z0-9][a-z0-9-]*)`/g;
const PROJECT_LOCAL_MENTION = /\bproject[- ]local(?:ly)?\b/i;
const CONDITIONAL_WORD = /\b(?:when|whenever|if|where|unless)\b/i;
const AVAILABILITY_WORD =
  /\b(?:is available|are available|is listed|are listed|lists|provides|is installed|is (?:driving|active|running|enabled)|if available)\b|\b(?:session|harness|environment|runtime)\b[^.]{0,60}\b(?:has|have|gives|provides|lists)\b/i;

function taskCallBodies(content) {
  const bodies = [];
  for (const match of content.matchAll(TASK_CALL_OPEN)) {
    const start = (match.index ?? 0) + match[0].length;
    let depth = 1;
    let end = start;
    while (end < content.length && end - start < 4000 && depth > 0) {
      const ch = content[end];
      if (ch === "(") depth += 1;
      else if (ch === ")") depth -= 1;
      end += 1;
    }
    bodies.push({ start, body: content.slice(start, end) });
  }
  return bodies;
}

export function extractReferences(content) {
  const references = [];
  const push = (referenceClass, reference, index) =>
    references.push({ referenceClass, reference, index });

  // Pass 1: skill-name
  for (const match of content.matchAll(LOAD_SKILLS_LIST)) {
    const listStart = (match.index ?? 0) + match[0].indexOf("[") + 1;
    for (const quoted of (match[1] ?? "").matchAll(QUOTED)) {
      push("skill-name", quoted[1] ?? "", listStart + (quoted.index ?? 0));
    }
  }
  for (const pattern of [SKILL_TOOL_CALL, SLASH_SKILL, SKILL_FILE_PATH, BACKTICK_THEN_SKILL, SKILL_THEN_BACKTICK]) {
    for (const match of content.matchAll(pattern)) {
      push("skill-name", match[1] ?? "", match.index ?? 0);
    }
  }

  // Pass 2: agent-name & category-name
  for (const match of content.matchAll(SUBAGENT_TYPE)) {
    push("agent-name", match[1] ?? "", match.index ?? 0);
  }
  for (const match of content.matchAll(AGENT_TYPE)) {
    push("agent-name", match[1] ?? "", match.index ?? 0);
  }
  for (const { start, body } of taskCallBodies(content)) {
    for (const match of body.matchAll(CATEGORY)) {
      push("category-name", match[1] ?? "", start + (match.index ?? 0));
    }
  }

  // Pass 3: skill-path
  for (const match of content.matchAll(SKILL_RELATIVE_PATH)) {
    push("skill-path", match[1] ?? "", match.index ?? 0);
  }

  return references;
}

function lineBounds(content, index) {
  const start = content.lastIndexOf("\n", index - 1) + 1;
  const newline = content.indexOf("\n", index);
  return { start, end: newline === -1 ? content.length : newline };
}

function lineNumber(content, index) {
  let line = 1;
  for (let cur = content.indexOf("\n"); cur !== -1 && cur < index; cur = content.indexOf("\n", cur + 1)) {
    line += 1;
  }
  return line;
}

function sentenceAround(content, index) {
  const { start, end } = lineBounds(content, index);
  const line = content.slice(start, end);
  const offset = index - start;
  const before = line.slice(0, offset);
  const lastBoundary = before.search(/[.!?]\s(?!.*[.!?]\s)/);
  const sentenceStart = lastBoundary === -1 ? 0 : lastBoundary + 2;
  const afterMatch = line.slice(offset).search(/[.!?](?:\s|$)/);
  return line.slice(sentenceStart, afterMatch === -1 ? line.length : offset + afterMatch + 1);
}

function isOptionalMention(content, index) {
  const sentence = sentenceAround(content, index);
  return (CONDITIONAL_WORD.test(sentence) && AVAILABILITY_WORD.test(sentence)) || /\bif available\b/i.test(sentence);
}

function owningSkill(content, index) {
  const paragraphStart = Math.max(content.lastIndexOf("\n\n", index), index - 600, 0);
  const window = content.slice(paragraphStart, index);
  let owner;
  for (const match of window.matchAll(SKILL_OWNER)) {
    owner = match[1] ?? match[2];
  }
  return owner;
}

function isTemplated(reference) {
  return /[<>{}*$[\]|]|\.\.\./.test(reference);
}

function cleanPath(reference) {
  return (reference.split("#", 1)[0] ?? "")
    .replace(/[),.:;'"]+$/, "")
    .replace(/:\d+(?::\d+)?$/, "");
}

function resolvesUnder(root, reference) {
  const target = cleanPath(reference);
  if (target.length === 0) return false;
  return existsSync(resolve(root, target));
}

export function auditFileContent({ filePath, content, skillRoot, repoRoot, catalog }) {
  const findings = [];
  const relativePath = toPortable(relative(repoRoot, filePath));

  for (const raw of extractReferences(content)) {
    const line = lineNumber(content, raw.index);

    if (raw.referenceClass === "skill-path") {
      if (!skillRoot || isTemplated(raw.reference)) continue;
      // Ignore synthetic scratch or example paths inside prose explanations of test fixtures
      const cleaned = cleanPath(raw.reference);
      if (cleaned.length === 0) continue;
      const owner = owningSkill(content, raw.index);
      const ownerRoots = owner === undefined ? [] : (catalog.skillRoots.get(owner) ?? []);
      if (ownerRoots.length > 0) {
        if (ownerRoots.some((r) => resolvesUnder(r, raw.reference))) continue;
        // Also check current skillRoot or dirname(filePath)
        if (resolvesUnder(skillRoot, raw.reference) || resolvesUnder(dirname(filePath), raw.reference) || resolvesUnder(repoRoot, raw.reference)) continue;
        findings.push({
          file: relativePath,
          line,
          referenceClass: "skill-path",
          reference: `${owner}:${cleaned}`,
          suggestion: suggestPath(ownerRoots[0], ownerRoots[0], cleaned),
        });
        continue;
      }
      if (
        resolvesUnder(skillRoot, raw.reference) ||
        resolvesUnder(dirname(filePath), raw.reference) ||
        resolvesUnder(repoRoot, raw.reference)
      ) {
        continue;
      }
      findings.push({
        file: relativePath,
        line,
        referenceClass: "skill-path",
        reference: cleaned,
        suggestion: suggestPath(skillRoot, dirname(filePath), cleaned),
      });
      continue;
    }

    const name = raw.reference.includes("/") ? raw.reference.split("/")[0] : raw.reference;
    if (PLACEHOLDER_NAMES.has(name) || isTemplated(name) || !SKILL_NAME_SHAPE.test(name)) continue;

    if (raw.referenceClass === "agent-name") {
      if (catalog.agents.has(name)) continue;
      findings.push({
        file: relativePath,
        line,
        referenceClass: "agent-name",
        reference: name,
        suggestion: suggestName(name, catalog.agents),
      });
      continue;
    }

    if (raw.referenceClass === "category-name") {
      if (catalog.categories.has(name)) continue;
      findings.push({
        file: relativePath,
        line,
        referenceClass: "category-name",
        reference: name,
        suggestion: suggestName(name, catalog.categories),
      });
      continue;
    }

    if (raw.referenceClass === "skill-name") {
      if (catalog.shippedSkills.has(name)) continue;
      const optional = isOptionalMention(content, raw.index);
      if (
        optional &&
        (catalog.optionalSkills.has(name) || PROJECT_LOCAL_MENTION.test(sentenceAround(content, raw.index)))
      ) {
        continue;
      }
      findings.push({
        file: relativePath,
        line,
        referenceClass: "skill-name",
        reference: name,
        suggestion: suggestName(name, catalog.shippedSkills),
      });
    }
  }

  return findings;
}

export function auditSkillReferences({ repoRoot = process.cwd(), customPaths = [], catalog: customCatalog } = {}) {
  const catalog = customCatalog ?? buildReferenceCatalog(repoRoot);
  const skillsBase = join(repoRoot, "assets/omo-latest/skills");
  const skillDirs =
    customPaths.length > 0
      ? customPaths.map((p) => (existsSync(join(skillsBase, p)) ? join(skillsBase, p) : resolve(repoRoot, p)))
      : listDirectories(skillsBase)
          .map((name) => join(skillsBase, name))
          .filter((dir) => existsSync(join(dir, "SKILL.md")));

  const findings = [];
  for (const skillDir of skillDirs) {
    const files = existsSync(skillDir) && statSync(skillDir).isDirectory()
      ? listFilesRecursive(skillDir).filter(
          (f) => f.endsWith(".md") && !NON_SHIPPED_FILE.test(toPortable(relative(repoRoot, f))),
        )
      : existsSync(skillDir)
        ? [skillDir]
        : [];
    const root = existsSync(join(skillDir, "SKILL.md")) ? skillDir : dirname(skillDir);
    for (const filePath of files) {
      const content = readFileSync(filePath, "utf8");
      findings.push(
        ...auditFileContent({
          filePath,
          content,
          skillRoot: root,
          repoRoot,
          catalog,
        }),
      );
    }
  }

  // Also run ulw-bench rules over ulw skills when no customPaths or matching customPaths are scanned
  for (const skillDir of skillDirs) {
    const name = basename(skillDir);
    if (["ultrawork", "ulw-loop", "ulw-execute", "ulw-plan", "ulw-research"].includes(name)) {
      const files = listFilesRecursive(skillDir).filter((f) => f.endsWith(".md") || f.endsWith(".mjs"));
      for (const file of files) {
        for (const f of checkUlwBenchFile(file, repoRoot)) {
          findings.push({
            file: f.file,
            line: f.line,
            referenceClass: `ulw-rule-${f.rule}`,
            reference: f.match,
            suggestion: f.message,
          });
        }
      }
    }
  }

  return findings;
}

export function formatFinding(f) {
  const hint = f.suggestion ? ` (did you mean "${f.suggestion}"?)` : "";
  return `${f.file}:${f.line}: [${f.referenceClass}] unresolved "${f.reference}"${hint}`;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2);
  let repoRoot = process.cwd();
  const customPaths = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--repo-root" && i + 1 < argv.length) {
      repoRoot = resolve(argv[++i]);
    } else if (!argv[i].startsWith("--")) {
      customPaths.push(argv[i]);
    }
  }
  const findings = auditSkillReferences({ repoRoot, customPaths });
  if (findings.length === 0) {
    console.log("check-skill-refs: 3-pass reference audit passed (0 unresolved references).");
    process.exit(0);
  }
  for (const f of findings) {
    console.log(formatFinding(f));
  }
  console.error(`check-skill-refs: found ${findings.length} unresolved reference(s).`);
  process.exit(1);
}
