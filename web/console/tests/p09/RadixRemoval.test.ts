import { access, readdir, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

async function sourceFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = await Promise.all(
    entries.map(async (entry) => {
      const path = join(directory, entry.name);
      return entry.isDirectory() ? sourceFiles(path) : /\.(?:ts|tsx)$/.test(entry.name) ? [path] : [];
    }),
  );
  return files.flat();
}

async function exists(path: string) {
  return access(path).then(() => true, () => false);
}

describe("P09 Radix removal", () => {
  it("removes every Radix dependency, lockfile entry, provider, and wrapper", async () => {
    const root = resolve(process.cwd());
    const packageJson = JSON.parse(await readFile(join(root, "package.json"), "utf8")) as {
      dependencies?: Record<string, string>;
      devDependencies?: Record<string, string>;
    };
    const packageLock = JSON.parse(await readFile(join(root, "package-lock.json"), "utf8")) as {
      packages?: Record<string, unknown>;
    };
    const sources = await sourceFiles(join(root, "src"));
    const allSource = (await Promise.all(sources.map((file) => readFile(file, "utf8")))).join("\n");
    const packages = { ...packageJson.dependencies, ...packageJson.devDependencies };

    expect(Object.keys(packages).filter((name) => name.startsWith("@radix-ui/"))).toEqual([]);
    expect(Object.keys(packageLock.packages ?? {}).filter((path) => path.includes("node_modules/@radix-ui/"))).toEqual([]);
    expect(allSource).not.toContain("@radix-ui/");
    expect(await exists(join(root, "src/components/ui"))).toBe(false);
  });
});
