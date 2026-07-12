import { afterEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";

Object.defineProperty(window, "matchMedia", {
  configurable: true,
  value: vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })),
});

class TestResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}

vi.stubGlobal("ResizeObserver", TestResizeObserver);
Element.prototype.scrollIntoView = vi.fn();

afterEach(() => cleanup());
