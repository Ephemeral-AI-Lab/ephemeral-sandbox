import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { CONSOLE_BREAKPOINTS, CONSOLE_Z_INDEX, ephemeralosTheme } from "@/theme";

function contrastRatio(foreground: string, background: string) {
  const luminance = (hex: string) => {
    const channels = hex.match(/[a-f\d]{2}/gi)!.map((channel) => Number.parseInt(channel, 16) / 255);
    const [red, green, blue] = channels.map((channel) =>
      channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };

  const [lighter, darker] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

describe("P01 EphemeralOS theme", () => {
  it("pins the canonical logo in a durable static source path", async () => {
    const bytes = await readFile(resolve(process.cwd(), "public/assets/images/logo.png"));
    expect(createHash("sha256").update(bytes).digest("hex")).toBe(
      "42a06f2cdf37becedf0b4436fdcb6fd8cee4e6a93120cfbd4cee727ba84fc81a",
    );
  });

  it("owns complete color, layout, motion, and stacking tokens", () => {
    for (const color of ["warm", "neutral", "eyeBlue", "success", "warning", "danger"] as const) {
      expect(ephemeralosTheme.colors[color]).toHaveLength(10);
    }

    expect(ephemeralosTheme.breakpoints).toMatchObject(CONSOLE_BREAKPOINTS);
    expect(ephemeralosTheme.other.zIndex).toMatchObject(CONSOLE_Z_INDEX);
    expect(ephemeralosTheme.respectReducedMotion).toBe(true);
    expect(ephemeralosTheme.defaultRadius).toBe("sm");
  });

  it("pins the CSS-module breakpoint mirror to the theme contract", async () => {
    const css = await readFile(
      resolve(process.cwd(), "tests/p00/p01-theme-fixture.module.css"),
      "utf8",
    );

    expect(CONSOLE_BREAKPOINTS.sm).toBe("48em");
    expect(css).toContain("Mirrors CONSOLE_BREAKPOINTS.sm (48em)");
    expect(css).toMatch(/@media \(max-width: 47\.99em\)/);
  });

  it("keeps essential text and focus colors above AA contrast on light surfaces", () => {
    expect(contrastRatio(ephemeralosTheme.black, ephemeralosTheme.white)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(ephemeralosTheme.colors.eyeBlue[7], ephemeralosTheme.white)).toBeGreaterThanOrEqual(
      4.5,
    );
    expect(contrastRatio(ephemeralosTheme.colors.success[7], ephemeralosTheme.white)).toBeGreaterThanOrEqual(
      4.5,
    );
    expect(contrastRatio(ephemeralosTheme.colors.warning[7], ephemeralosTheme.white)).toBeGreaterThanOrEqual(
      4.5,
    );
    expect(contrastRatio(ephemeralosTheme.colors.danger[7], ephemeralosTheme.white)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(ephemeralosTheme.colors.gray[6], ephemeralosTheme.white)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(ephemeralosTheme.colors.red[6], ephemeralosTheme.white)).toBeGreaterThanOrEqual(4.5);
  });
});
